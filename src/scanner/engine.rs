//! Scanner engine logic.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey};
use rayon::prelude::*;
use tokio::sync::watch;
use walkdir::WalkDir;

use crate::db::{DbPool, MusicFolderRepository, MusicRepoError};
use crate::models::music::MusicFolder;
use crate::paths::resolve_cover_art_dir;
use crate::scanner::state::{ScanPhase, ScanState, ScanStateHandle};
use crate::scanner::types::{
    AUDIO_EXTENSIONS, BATCH_SIZE, COVER_ART_FILENAMES, DEFAULT_AUTO_SCAN_INTERVAL_SECS,
    IMAGE_EXTENSIONS, PreparedTrack, ScanError, ScanMode, ScanResult, ScannedTrack,
};

/// Music library scanner.
#[derive(Debug)]
pub struct Scanner {
    pool: DbPool,
    cover_art_dir: PathBuf,
}

/// Auto-scanner that runs periodic scans in the background.
#[derive(Clone, Debug)]
pub struct AutoScanner {
    pool: DbPool,
    cover_art_dir: PathBuf,
    interval: Duration,
    scan_state: ScanStateHandle,
}

#[derive(Debug, Clone)]
struct DiscoveredFile {
    path: PathBuf,
    file_size: u64,
    file_modified_at: Option<i64>,
    master_of: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct ExistingFile {
    modified_at: Option<i64>,
    size: i64,
    r#type: ExistingFileType,
}

#[derive(Debug, Clone)]
enum ExistingFileType {
    LeafFile { master: Option<String> },
    MasterFile { leafs: HashSet<String> },
}

enum AutoScanOutcome {
    Completed(ScanResult),
}

impl Scanner {
    /// Create a new scanner.
    #[must_use]
    pub fn new(pool: DbPool) -> Self {
        let cover_art_dir = resolve_cover_art_dir();

        Self {
            pool,
            cover_art_dir,
        }
    }

    /// Create a new scanner with a custom cover art directory.
    #[must_use]
    pub const fn with_cover_art_dir(pool: DbPool, cover_art_dir: PathBuf) -> Self {
        Self {
            pool,
            cover_art_dir,
        }
    }

    /// Ensure cover art cache directory exists.
    fn ensure_cover_art_dir(&self) -> Result<(), ScanError> {
        if !self.cover_art_dir.exists() {
            fs::create_dir_all(&self.cover_art_dir)?;
        }
        Ok(())
    }

    /// Save cover art to cache and return the cover art ID.
    fn save_cover_art(&self, data: &[u8], mime: &str) -> Result<String, ScanError> {
        use md5::{Digest, Md5};

        // Generate hash-based ID for the cover art
        let mut hasher = Md5::new();
        hasher.update(data);
        let hash = hex::encode(hasher.finalize());

        // Determine file extension from MIME type
        let ext = match mime {
            "image/webp" => "webp",
            "image/png" => "png",
            "image/gif" => "gif",
            "image/bmp" => "bmp",
            "image/tiff" => "tiff",
            _ => "jpg", // Default to JPEG
        };

        let filename = format!("{hash}.{ext}");
        let filepath = self.cover_art_dir.join(&filename);

        // Only write if file doesn't already exist (same content = same hash)
        if !filepath.exists() {
            fs::write(&filepath, data)?;
        }

        // Return just the hash as the cover art ID
        Ok(hash)
    }

    fn image_mime_from_ext(ext: &str) -> &'static str {
        match ext {
            "png" => "image/png",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            "webp" => "image/webp",
            _ => "image/jpeg",
        }
    }

    /// Get the cover art cache directory path.
    #[must_use]
    pub fn cover_art_dir(&self) -> &Path {
        &self.cover_art_dir
    }

    /// Look for external cover art file in the given directory.
    /// Tries common filenames like cover.jpg, folder.png, etc.
    /// Returns the cover art data and MIME type if found.
    fn find_external_cover_art(dir: &Path) -> Option<(Vec<u8>, String)> {
        // Try each common filename with each supported extension
        for filename in COVER_ART_FILENAMES {
            for ext in IMAGE_EXTENSIONS {
                let path = dir.join(format!("{filename}.{ext}"));
                if path.exists()
                    && path.is_file()
                    && let Ok(data) = fs::read(&path)
                {
                    return Some((data, Self::image_mime_from_ext(ext).to_string()));
                }
            }
        }

        // Also try case-insensitive matching as a fallback
        // Some albums might have "Cover.JPG" or "FOLDER.PNG"
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.filter_map(std::result::Result::ok) {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                let filename = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_lowercase);
                let ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(str::to_lowercase);

                if let (Some(name), Some(extension)) = (filename, ext)
                    && COVER_ART_FILENAMES.contains(&name.as_str())
                    && IMAGE_EXTENSIONS.contains(&extension.as_str())
                    && let Ok(data) = fs::read(&path)
                {
                    return Some((data, Self::image_mime_from_ext(&extension).to_string()));
                }
            }
        }

        None
    }

    /// Scan all enabled music folders (full scan).
    pub fn scan_all(&self) -> Result<ScanResult, ScanError> {
        self.scan_all_with_options(None, ScanMode::Full)
    }

    /// Scan all enabled music folders (incremental - only changed files).
    pub fn scan_all_incremental(&self) -> Result<ScanResult, ScanError> {
        self.scan_all_with_options(None, ScanMode::Incremental)
    }

    /// Scan all enabled music folders with optional progress tracking.
    ///
    /// If a `ScanState` is provided, the count will be updated as tracks are processed.
    pub fn scan_all_with_state(&self, state: Option<&ScanState>) -> Result<ScanResult, ScanError> {
        self.scan_all_with_options(state, ScanMode::Full)
    }

    /// Scan all enabled music folders with optional progress tracking and scan mode.
    pub fn scan_all_with_options(
        &self,
        state: Option<&ScanState>,
        mode: ScanMode,
    ) -> Result<ScanResult, ScanError> {
        let folder_repo = MusicFolderRepository::new(self.pool.clone());
        let folders = folder_repo.find_enabled()?;

        if folders.is_empty() {
            return Err(ScanError::NoMusicFolders);
        }

        let mut total_result = ScanResult::default();

        for folder in &folders {
            // Update scan state with current folder
            if let Some(s) = state {
                s.set_current_folder(Some(folder.name.clone()));
            }

            println!(
                "Scanning folder: {} ({}) [mode: {:?}]",
                folder.name, folder.path, mode
            );
            match self.scan_folder_with_options(folder, state, mode) {
                Ok(result) => {
                    total_result.tracks_found += result.tracks_found;
                    total_result.tracks_added += result.tracks_added;
                    total_result.tracks_updated += result.tracks_updated;
                    total_result.tracks_skipped += result.tracks_skipped;
                    total_result.tracks_removed += result.tracks_removed;
                    total_result.tracks_failed += result.tracks_failed;
                    total_result.artists_added += result.artists_added;
                    total_result.albums_added += result.albums_added;
                    total_result.cover_art_saved += result.cover_art_saved;
                }
                Err(e) => {
                    eprintln!("Error scanning folder {}: {}", folder.name, e);
                    return Err(e);
                }
            }
        }

        // Clean up orphaned artists and albums after scanning all folders
        if let Some(s) = state {
            s.set_phase(ScanPhase::Cleaning);
            s.set_current_folder(None);
        }

        if let Err(e) = self.cleanup_orphans() {
            return Err(ScanError::Cleanup(format!(
                "Failed to cleanup orphaned records: {e}"
            )));
        }

        Ok(total_result)
    }

    /// Scan a specific music folder by ID (full scan).
    pub fn scan_folder_by_id(&self, folder_id: i32) -> Result<ScanResult, ScanError> {
        self.scan_folder_by_id_with_mode(folder_id, ScanMode::Full)
    }

    /// Scan a specific music folder by ID with scan mode.
    pub fn scan_folder_by_id_with_mode(
        &self,
        folder_id: i32,
        mode: ScanMode,
    ) -> Result<ScanResult, ScanError> {
        self.scan_folder_by_id_with_state_and_mode(folder_id, None, mode)
    }

    /// Scan a specific music folder by ID with optional progress tracking and scan mode.
    pub fn scan_folder_by_id_with_state_and_mode(
        &self,
        folder_id: i32,
        state: Option<&ScanState>,
        mode: ScanMode,
    ) -> Result<ScanResult, ScanError> {
        let folder_repo = MusicFolderRepository::new(self.pool.clone());
        let folder = folder_repo
            .find_by_id(folder_id)?
            .ok_or_else(|| ScanError::FolderNotFound(folder_id.to_string()))?;

        println!(
            "Scanning folder: {} ({}) [mode: {:?}]",
            folder.name, folder.path, mode
        );
        self.scan_folder_with_options(&folder, state, mode)
    }

    /// Scan a single music folder with optional progress tracking and scan mode.
    fn scan_folder_with_options(
        &self,
        folder: &MusicFolder,
        state: Option<&ScanState>,
        mode: ScanMode,
    ) -> Result<ScanResult, ScanError> {
        let mut result = ScanResult::default();
        let folder_path = Path::new(&folder.path);

        if !folder_path.exists() {
            return Err(ScanError::FolderNotFound(folder.path.clone()));
        }

        // Set discovery phase
        if let Some(s) = state {
            s.set_phase(ScanPhase::Discovering);
        }

        // Get existing files in this folder for incremental scanning
        let existing_files = self.get_existing_files(folder.id)?;

        // Collect all audio files on disk
        let (discovered_files, discovered_paths) = Self::discover_files_with_paths(folder_path);
        result.tracks_found = discovered_files.len();

        // Find deleted files (in database but not on disk)
        let deleted_paths: HashMap<_, _> = existing_files
            .iter()
            .filter(|(path, _)| !discovered_paths.contains(*path))
            .map(|(path, file)| (path.clone(), file.clone()))
            .collect();

        let (files_to_process, skipped_unchanged) =
            Self::split_discovered_files_for_mode(discovered_files, &existing_files, mode);

        // Set total count now that we know how many files to process
        if let Some(s) = state {
            // Add to total (accumulates across folders)
            let current_total = s.get_total();
            s.set_total(current_total + result.tracks_found as u64);
            if skipped_unchanged > 0 {
                let current_count = s.get_count();
                s.set_count(current_count + skipped_unchanged as u64);
            }
            s.set_phase(ScanPhase::Processing);
        }

        println!("  Found {} audio files on disk", result.tracks_found);
        if skipped_unchanged > 0 {
            println!("  Skipping {skipped_unchanged} unchanged files");
        }

        // Read metadata in parallel for files that need processing.
        let folder_path_str = folder.path.clone();
        let tracks: Vec<ScannedTrack> = files_to_process
            .par_iter()
            .filter_map(
                |file| match Self::read_track_metadata_static(file, &folder_path_str) {
                    Ok(track) => Some(track),
                    Err(e) => {
                        eprintln!("  Warning: Failed to read {}: {}", file.path.display(), e);
                        None
                    }
                },
            )
            .collect();

        if !deleted_paths.is_empty() {
            let dirty_album_ids: HashSet<i32> = deleted_paths
                .iter()
                .filter_map(|path| existing_files.get(path).and_then(|song| song.album_id))
                .collect();
            println!(
                "  Removing {} deleted files from database",
                deleted_paths.len()
            );
            result.tracks_removed = self.remove_deleted_songs(&deleted_paths)?;
            self.update_album_stats_for_ids(&dirty_album_ids)?;
        }

        // Process tracks and populate database
        let processed = self.process_tracks_with_options(
            folder,
            tracks,
            &existing_files,
            state,
            skipped_unchanged,
        )?;

        result.artists_added = processed.artists_added;
        result.albums_added = processed.albums_added;
        result.tracks_added = processed.tracks_added;
        result.tracks_updated = processed.tracks_updated;
        result.tracks_skipped = processed.tracks_skipped;
        result.tracks_failed = processed.tracks_failed;
        result.cover_art_saved = processed.cover_art_saved;

        Ok(result)
    }

    /// Get existing songs in a folder from the database.
    fn get_existing_files(
        &self,
        folder_id: i32,
    ) -> Result<HashMap<String, ExistingFile>, ScanError> {
        use crate::db::schema::files;
        use diesel::prelude::*;

        let mut conn = self.pool.get().map_err(MusicRepoError::from)?;
        /*
                select f.*, mstr.*
                from files f
                left join files mstr on f.master = mstr.id
                where f.foulderId = ?


        let results = emp
                .left_join(
                    supervisor.on(emp
                        .field(employees::supervisor_id)
                        .eq(supervisor.field(employees::id).nullable()))
                )
                .select((
                    emp.fields((employees::id, employees::name, employees::supervisor_id)),
                    // Fields from the right side of a LEFT JOIN must be explicitly .nullable()
                    supervisor.fields((employees::id, employees::name, employees::supervisor_id)).nullable(),
                ))
                .load::<(Employee, Option<Employee>)>(conn)?;
                 */
        let (master, file) = diesel::alias!(files as master, files as file);
        let existing = file
            .left_join(
                master.on(file
                    .field(files::master)
                    .eq(master.field(files::id).nullable())),
            )
            .filter(file.field(files::music_folder_id).eq(folder_id))
            .select((
                file.fields((files::path, files::modified_at, files::size)),
                master.field(files::path).nullable(),
            ))
            .load(&mut conn)
            .map_err(MusicRepoError::from)?;

        let mut map = HashMap::new();
        for ((path, modified_at, size), master) in existing.into_iter() {
            use std::collections::hash_map::Entry;
            let master: Option<String> = master;
            let path: String = path;

            if let Some(mater_path) = master.clone() {
                match map.entry(mater_path) {
                    Entry::Occupied(mut file) => {
                        let file: &mut ExistingFile = file.get_mut();
                        file.add_leaf(path.clone());
                    }
                    Entry::Vacant(vacant_entry) => {
                        vacant_entry.insert(ExistingFile {
                            r#type: ExistingFileType::MasterFile {
                                leafs: std::iter::once(path.clone()).collect(),
                            },
                            modified_at: None,
                            size: 0,
                        });
                    }
                }
            }
            match map.entry(path) {
                Entry::Occupied(mut file) => {
                    let file: &mut ExistingFile = file.get_mut();
                    file.modified_at = modified_at;
                    file.size = size;
                }
                Entry::Vacant(vacant_entry) => {
                    vacant_entry.insert(ExistingFile {
                        modified_at,
                        size,
                        r#type: ExistingFileType::LeafFile {
                            master: (master as Option<String>).clone(),
                        },
                    });
                }
            }

        }

        Ok(map)
    }

    /// Remove songs that no longer exist on disk.
    fn remove_deleted_songs(&self, paths: &[String]) -> Result<usize, ScanError> {
        use crate::db::schema::songs;
        use diesel::prelude::*;

        let mut conn = self.pool.get().map_err(MusicRepoError::from)?;

        let deleted = diesel::delete(songs::table.filter(songs::path.eq_any(paths)))
            .execute(&mut conn)
            .map_err(MusicRepoError::from)?;

        Ok(deleted)
    }

    /// Clean up orphaned artists and albums (those with no songs).
    fn cleanup_orphans(&self) -> Result<(), ScanError> {
        use diesel::prelude::*;

        let mut conn = self.pool.get().map_err(MusicRepoError::from)?;

        // Delete albums with no songs
        diesel::sql_query(
            "DELETE FROM albums WHERE id NOT IN (SELECT DISTINCT album_id FROM songs WHERE album_id IS NOT NULL)"
        )
        .execute(&mut conn)
        .map_err(MusicRepoError::from)?;

        // Delete artists with no songs and no albums
        diesel::sql_query(
            "DELETE FROM artists WHERE id NOT IN (SELECT DISTINCT artist_id FROM songs WHERE artist_id IS NOT NULL) AND id NOT IN (SELECT DISTINCT artist_id FROM albums WHERE artist_id IS NOT NULL)"
        )
        .execute(&mut conn)
        .map_err(MusicRepoError::from)?;

        Ok(())
    }

    /// Discover all audio files in a directory, also returning the set of discovered paths.
    fn discover_files_with_paths(folder_path: &Path) -> (Vec<DiscoveredFile>, HashSet<String>) {
        // First, collect all audio file paths (fast, sequential walk)
        let audio_files: Vec<PathBuf> = WalkDir::new(folder_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry),
                Err(error) => {
                    tracing::warn!(error = %error, "Failed to walk scanner path");
                    None
                }
            })
            .filter(|entry| entry.path().is_file())
            .filter_map(|entry| {
                let path = entry.into_path();
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_lowercase);

                match ext {
                    Some(ext) if AUDIO_EXTENSIONS.contains(&ext.as_str()) => Some(path),
                    _ => None,
                }
            })
            .collect();

        // Build paths set
        let paths: HashSet<String> = audio_files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        let discovered_files: Vec<DiscoveredFile> = audio_files
            .into_iter()
            .filter_map(|path| {
                let metadata = match fs::metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(e) => {
                        eprintln!(
                            "  Warning: Failed to read metadata {}: {}",
                            path.display(),
                            e
                        );
                        return None;
                    }
                };

                let file_modified_at = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

                Some(DiscoveredFile {
                    path,
                    file_size: metadata.len(),
                    file_modified_at,
                    master_of: Default::default(),
                })
            })
            .collect();

        (discovered_files, paths)
    }

    fn split_discovered_files_for_mode(
        discovered_files: Vec<DiscoveredFile>,
        existing_songs: &HashMap<String, ExistingFile>,
        mode: ScanMode,
    ) -> (HashSet<DiscoveredFile>, usize) {
        if mode == ScanMode::Full {
            return (discovered_files.iter().collect(), 0);
        }

        let mut to_process = Vec::with_capacity(discovered_files.len());
        let mut skipped_unchanged = 0;

        for file in discovered_files {
            let path_str = file.path.to_string_lossy();
            let unchanged = existing_songs
                .get(path_str.as_ref())
                .is_some_and(|stored| {
                    matches!((stored.modified_at, file.file_modified_at), (Some(stored), Some(current)) if stored == current)
                        && stored.size == i64::try_from(file.file_size).unwrap_or(i64::MAX)
                });

            if unchanged {
                skipped_unchanged += 1;
            } else {
                to_process.push(file);
            }
        }

        (to_process, skipped_unchanged)
    }

    /// Static version of `read_track_metadata` for use with rayon (no &self needed).
    fn read_track_metadata_static(
        file: &DiscoveredFile,
        folder_path: &str,
    ) -> Result<ScannedTrack, ScanError> {
        let path = &file.path;
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .unwrap_or_default();

        // Get parent path relative to music folder
        let parent_path = path
            .parent()
            .map(|p| p.strip_prefix(folder_path).unwrap_or(p).to_path_buf())
            .unwrap_or_default();

        // Read audio tags
        let tagged_file = lofty::read_from_path(path).map_err(|error| ScanError::Metadata {
            path: path.clone(),
            message: error.to_string(),
        })?;

        let properties = tagged_file.properties();
        let duration_secs = u32::try_from(properties.duration().as_secs()).unwrap_or(u32::MAX);
        let bit_rate = properties.audio_bitrate();
        let bit_depth = properties.bit_depth();
        let sample_rate = properties.sample_rate();
        let channels = properties.channels();

        // Get tags (try primary tag first, then any available)
        let tag = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag());

        let (title, artist, album, album_artist, track_number, disc_number, year, genre) = tag
            .map_or((None, None, None, None, None, None, None, None), |tag| {
                (
                    tag.title().map(|s| s.to_string()),
                    tag.artist().map(|s| s.to_string()),
                    tag.album().map(|s| s.to_string()),
                    tag.get_string(&ItemKey::AlbumArtist)
                        .map(std::string::ToString::to_string),
                    tag.track(),
                    tag.disk(),
                    tag.year(),
                    tag.genre().map(|s| s.to_string()),
                )
            });

        // Use filename as title if no tag
        let title = title.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string()
        });

        let content_type = match extension.as_str() {
            "mp3" => "audio/mpeg",
            "flac" => "audio/flac",
            "ogg" => "audio/ogg",
            "opus" => "audio/opus",
            "m4a" | "aac" => "audio/mp4",
            "wav" => "audio/wav",
            "wma" => "audio/x-ms-wma",
            "aiff" => "audio/aiff",
            "ape" => "audio/ape",
            "wv" => "audio/wavpack",
            _ => "audio/unknown",
        }
        .to_string();

        Ok(ScannedTrack {
            path: path.clone(),
            parent_path,
            file_size: file.file_size,
            content_type,
            suffix: extension,
            title,
            artist,
            album,
            album_artist,
            track_number,
            disc_number,
            year,
            genre,
            duration_secs,
            bit_rate,
            bit_depth,
            sample_rate,
            channels,
            file_modified_at: file.file_modified_at,
        })
    }

    fn extract_embedded_cover_art(path: &Path) -> Option<(Vec<u8>, String)> {
        let tagged_file = lofty::read_from_path(path).ok()?;
        let tag = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag())?;
        let picture = tag.pictures().first()?;

        let mime = match picture.mime_type() {
            Some(lofty::picture::MimeType::Png) => "image/png",
            Some(lofty::picture::MimeType::Gif) => "image/gif",
            Some(lofty::picture::MimeType::Bmp) => "image/bmp",
            Some(lofty::picture::MimeType::Tiff) => "image/tiff",
            _ => "image/jpeg",
        };

        Some((picture.data().to_vec(), mime.to_string()))
    }

    /// Process scanned tracks and populate the database with options.
    #[expect(
        clippy::too_many_lines,
        reason = "Track ingest handles dedupe, cover art, and DB upserts in one transaction path"
    )]
    fn process_tracks_with_options(
        &self,
        folder: &MusicFolder,
        tracks: Vec<ScannedTrack>,
        existing_songs: &HashMap<String, ExistingFile>,
        state: Option<&ScanState>,
        initial_tracks_skipped: usize,
    ) -> Result<ScanResult, ScanError> {
        use crate::db::schema::{albums, artists, songs};
        use diesel::prelude::*;

        // Ensure cover art directory exists
        self.ensure_cover_art_dir()?;

        let mut conn = self.pool.get().map_err(MusicRepoError::from)?;

        // Pre-load all existing artists into cache (much faster than individual lookups)
        let mut artist_cache: HashMap<String, i32> = artists::table
            .select((artists::name, artists::id))
            .load::<(String, i32)>(&mut conn)
            .map_err(MusicRepoError::from)?
            .into_iter()
            .collect();

        // Pre-load all existing albums into cache
        let mut album_cache: HashMap<(String, Option<i32>), i32> = albums::table
            .select((albums::name, albums::artist_id, albums::id))
            .load::<(String, Option<i32>, i32)>(&mut conn)
            .map_err(MusicRepoError::from)?
            .into_iter()
            .map(|(name, artist_id, id)| ((name, artist_id), id))
            .collect();

        // Pre-load album cover art hashes
        let mut album_cover_art_cache: HashMap<i32, Option<String>> = albums::table
            .select((albums::id, albums::cover_art))
            .load::<(i32, Option<String>)>(&mut conn)
            .map_err(MusicRepoError::from)?
            .into_iter()
            .collect();

        // Cache for external cover art per directory (None = already checked, no cover art found)
        let mut dir_cover_art_cache: HashMap<PathBuf, Option<(Vec<u8>, String)>> = HashMap::new();

        let mut result = ScanResult {
            tracks_found: tracks.len(),
            tracks_skipped: initial_tracks_skipped,
            ..ScanResult::default()
        };
        let mut dirty_album_ids: HashSet<i32> = HashSet::new();

        // Collect unique new artists and albums first (avoid duplicate inserts)
        let mut new_artists: HashSet<String> = HashSet::new();

        // First pass: collect all unique new artists
        for track in &tracks {
            let artist_name = track
                .album_artist
                .as_ref()
                .or(track.artist.as_ref())
                .cloned();

            if let Some(ref name) = artist_name
                && !artist_cache.contains_key(name)
            {
                new_artists.insert(name.clone());
            }
        }

        // Batch insert new artists in a transaction
        if !new_artists.is_empty() {
            conn.transaction::<_, diesel::result::Error, _>(|conn| {
                for name in &new_artists {
                    diesel::insert_into(artists::table)
                        .values(artists::name.eq(name))
                        .on_conflict_do_nothing()
                        .execute(conn)?;
                }
                Ok(())
            })
            .map_err(MusicRepoError::from)?;

            // Reload artist cache to get new IDs
            let new_artist_ids: Vec<(String, i32)> = artists::table
                .filter(artists::name.eq_any(&new_artists))
                .select((artists::name, artists::id))
                .load(&mut conn)
                .map_err(MusicRepoError::from)?;

            for (name, id) in new_artist_ids {
                if !artist_cache.contains_key(&name) {
                    result.artists_added += 1;
                }
                artist_cache.insert(name, id);
            }
        }

        let mut prepared_tracks: Vec<PreparedTrack> = Vec::with_capacity(tracks.len());

        // Second pass: resolve albums and prepare tracks
        for track in tracks {
            let path_str = track.path.to_string_lossy().to_string();

            // Get artist ID from cache
            let artist_name = track
                .album_artist
                .as_ref()
                .or(track.artist.as_ref())
                .cloned();

            let artist_id = artist_name
                .as_ref()
                .and_then(|name| artist_cache.get(name).copied());

            // Get or create album
            let album_id = if let Some(ref album_name) = track.album {
                let cache_key = (album_name.clone(), artist_id);

                if let Some(&id) = album_cache.get(&cache_key) {
                    Some(id)
                } else {
                    // Insert new album
                    diesel::insert_into(albums::table)
                        .values((
                            albums::name.eq(album_name),
                            albums::artist_id.eq(artist_id),
                            albums::artist_name.eq(&artist_name),
                            albums::year
                                .eq(track.year.map(|y| i32::try_from(y).unwrap_or(i32::MAX))),
                            albums::genre.eq(&track.genre),
                        ))
                        .on_conflict_do_nothing()
                        .execute(&mut conn)
                        .map_err(MusicRepoError::from)?;

                    // Get the album ID
                    let mut query = albums::table
                        .filter(albums::name.eq(album_name))
                        .into_boxed();
                    if let Some(aid) = artist_id {
                        query = query.filter(albums::artist_id.eq(aid));
                    } else {
                        query = query.filter(albums::artist_id.is_null());
                    }

                    let album_row: Option<(i32, Option<String>)> = query
                        .select((albums::id, albums::cover_art))
                        .first(&mut conn)
                        .optional()
                        .map_err(MusicRepoError::from)?;

                    if let Some((id, existing_cover)) = album_row {
                        if !album_cache.contains_key(&cache_key) {
                            result.albums_added += 1;
                        }
                        album_cache.insert(cache_key, id);
                        album_cover_art_cache.insert(id, existing_cover);
                        Some(id)
                    } else {
                        None
                    }
                }
            } else {
                None
            };

            // Handle cover art
            let album_cover_art_id = if let Some(album_id) = album_id {
                let existing_cover_art = album_cover_art_cache.get(&album_id).cloned().flatten();

                if existing_cover_art.is_none() {
                    let art_source: Option<(Vec<u8>, String)> =
                        if let Some(embedded_art) = Self::extract_embedded_cover_art(&track.path) {
                            Some(embedded_art)
                        } else if let Some(parent_dir) = track.path.parent() {
                            let parent_buf = parent_dir.to_path_buf();
                            dir_cover_art_cache
                                .entry(parent_buf)
                                .or_insert_with(|| Self::find_external_cover_art(parent_dir))
                                .clone()
                        } else {
                            None
                        };

                    if let Some((art_data, art_mime)) = art_source {
                        match self.save_cover_art(&art_data, &art_mime) {
                            Ok(cover_art_hash) => {
                                if let Err(e) =
                                    diesel::update(albums::table.filter(albums::id.eq(album_id)))
                                        .set(albums::cover_art.eq(&cover_art_hash))
                                        .execute(&mut conn)
                                {
                                    eprintln!("  Warning: Failed to update album cover art: {e}");
                                    None
                                } else {
                                    album_cover_art_cache
                                        .insert(album_id, Some(cover_art_hash.clone()));
                                    result.cover_art_saved += 1;
                                    Some(cover_art_hash)
                                }
                            }
                            Err(e) => {
                                eprintln!("  Warning: Failed to save cover art: {e}");
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    existing_cover_art
                }
            } else {
                None
            };

            let is_update = existing_songs.contains_key(&path_str);

            prepared_tracks.push(PreparedTrack {
                track,
                path_str,
                artist_id,
                album_id,
                cover_art: album_cover_art_id,
                is_update,
            });
        }

        // Process songs in batches within transactions
        for batch in prepared_tracks.chunks(BATCH_SIZE) {
            conn.transaction::<_, MusicRepoError, _>(|conn| {
                for prepared in batch {
                    let query_result =
                        if prepared.is_update {
                            diesel::update(songs::table.filter(songs::path.eq(&prepared.path_str)))
                                .set((
                                    songs::title.eq(&prepared.track.title),
                                    songs::album_id.eq(prepared.album_id),
                                    songs::artist_id.eq(prepared.artist_id),
                                    songs::artist_name.eq(&prepared.track.artist),
                                    songs::album_name.eq(&prepared.track.album),
                                    songs::file_size
                                        .eq(i64::try_from(prepared.track.file_size)
                                            .unwrap_or(i64::MAX)),
                                    songs::duration.eq(i32::try_from(prepared.track.duration_secs)
                                        .unwrap_or(i32::MAX)),
                                    songs::bit_rate.eq(prepared
                                        .track
                                        .bit_rate
                                        .map(|b| i32::try_from(b).unwrap_or(i32::MAX))),
                                    songs::bit_depth.eq(prepared.track.bit_depth.map(i32::from)),
                                    songs::sampling_rate.eq(prepared
                                        .track
                                        .sample_rate
                                        .map(|s| i32::try_from(s).unwrap_or(i32::MAX))),
                                    songs::channel_count.eq(prepared.track.channels.map(i32::from)),
                                    songs::track_number.eq(prepared
                                        .track
                                        .track_number
                                        .map(|t| i32::try_from(t).unwrap_or(i32::MAX))),
                                    songs::disc_number.eq(prepared
                                        .track
                                        .disc_number
                                        .map(|d| i32::try_from(d).unwrap_or(i32::MAX))),
                                    songs::year.eq(prepared
                                        .track
                                        .year
                                        .map(|y| i32::try_from(y).unwrap_or(i32::MAX))),
                                    songs::genre.eq(&prepared.track.genre),
                                    songs::cover_art.eq(&prepared.cover_art),
                                    songs::file_modified_at.eq(prepared.track.file_modified_at),
                                    songs::updated_at.eq(diesel::dsl::now),
                                ))
                                .execute(conn)
                        } else {
                            diesel::insert_into(songs::table)
                                .values((
                                    songs::title.eq(&prepared.track.title),
                                    songs::album_id.eq(prepared.album_id),
                                    songs::artist_id.eq(prepared.artist_id),
                                    songs::artist_name.eq(&prepared.track.artist),
                                    songs::album_name.eq(&prepared.track.album),
                                    songs::music_folder_id.eq(folder.id),
                                    songs::path.eq(&prepared.path_str),
                                    songs::parent_path
                                        .eq(prepared.track.parent_path.to_string_lossy()),
                                    songs::file_size
                                        .eq(i64::try_from(prepared.track.file_size)
                                            .unwrap_or(i64::MAX)),
                                    songs::content_type.eq(&prepared.track.content_type),
                                    songs::suffix.eq(&prepared.track.suffix),
                                    songs::duration.eq(i32::try_from(prepared.track.duration_secs)
                                        .unwrap_or(i32::MAX)),
                                    songs::bit_rate.eq(prepared
                                        .track
                                        .bit_rate
                                        .map(|b| i32::try_from(b).unwrap_or(i32::MAX))),
                                    songs::bit_depth.eq(prepared.track.bit_depth.map(i32::from)),
                                    songs::sampling_rate.eq(prepared
                                        .track
                                        .sample_rate
                                        .map(|s| i32::try_from(s).unwrap_or(i32::MAX))),
                                    songs::channel_count.eq(prepared.track.channels.map(i32::from)),
                                    songs::track_number.eq(prepared
                                        .track
                                        .track_number
                                        .map(|t| i32::try_from(t).unwrap_or(i32::MAX))),
                                    songs::disc_number.eq(prepared
                                        .track
                                        .disc_number
                                        .map(|d| i32::try_from(d).unwrap_or(i32::MAX))),
                                    songs::year.eq(prepared
                                        .track
                                        .year
                                        .map(|y| i32::try_from(y).unwrap_or(i32::MAX))),
                                    songs::genre.eq(&prepared.track.genre),
                                    songs::cover_art.eq(&prepared.cover_art),
                                    songs::file_modified_at.eq(prepared.track.file_modified_at),
                                ))
                                .execute(conn)
                        };

                    let changed = query_result.map_err(MusicRepoError::from)?;
                    if changed == 0 {
                        return Err(MusicRepoError::new(
                            crate::db::repo::error::MusicRepoErrorKind::NotFound,
                            format!("track disappeared during ingest: {}", prepared.path_str),
                        ));
                    }
                }
                Ok(())
            })?;

            // Update counters and progress
            for prepared in batch {
                if prepared.is_update {
                    result.tracks_updated += 1;
                } else {
                    result.tracks_added += 1;
                }
                if let Some(album_id) = prepared.album_id {
                    dirty_album_ids.insert(album_id);
                }
                if let Some(state) = state {
                    state.increment_count();
                }
            }
        }

        // Update album song counts and durations for dirty albums only
        Self::update_album_stats(&mut conn, &dirty_album_ids)?;

        Ok(result)
    }

    /// Update album statistics (song count, duration) for dirty albums only.
    fn update_album_stats(
        conn: &mut diesel::SqliteConnection,
        dirty_album_ids: &std::collections::HashSet<i32>,
    ) -> Result<(), ScanError> {
        use diesel::prelude::*;

        if dirty_album_ids.is_empty() {
            return Ok(());
        }

        let id_list: Vec<String> = dirty_album_ids.iter().map(i32::to_string).collect();
        let ids = id_list.join(",");
        let sql = format!(
            "
            UPDATE albums SET
                song_count = (SELECT COUNT(*) FROM songs WHERE songs.album_id = albums.id),
                duration = (SELECT COALESCE(SUM(duration), 0) FROM songs WHERE songs.album_id = albums.id),
                updated_at = CURRENT_TIMESTAMP
            WHERE id IN ({ids})
            "
        );

        diesel::sql_query(sql)
            .execute(conn)
            .map_err(MusicRepoError::from)?;

        Ok(())
    }

    fn update_album_stats_for_ids(&self, dirty_album_ids: &HashSet<i32>) -> Result<(), ScanError> {
        let mut conn = self.pool.get().map_err(MusicRepoError::from)?;
        Self::update_album_stats(&mut conn, dirty_album_ids)
    }
}

impl AutoScanner {
    /// Create a new auto-scanner with default interval (5 minutes).
    #[must_use]
    pub fn new(pool: DbPool, scan_state: ScanStateHandle) -> Self {
        let cover_art_dir = resolve_cover_art_dir();

        Self {
            pool,
            cover_art_dir,
            interval: Duration::from_secs(DEFAULT_AUTO_SCAN_INTERVAL_SECS),
            scan_state,
        }
    }

    /// Create a new auto-scanner with a custom interval.
    #[must_use]
    pub fn with_interval(pool: DbPool, scan_state: ScanStateHandle, interval_secs: u64) -> Self {
        let cover_art_dir = resolve_cover_art_dir();

        Self {
            pool,
            cover_art_dir,
            interval: Duration::from_secs(interval_secs),
            scan_state,
        }
    }

    /// Start the auto-scanner in the background.
    /// Returns a handle that can be used to stop the scanner.
    #[must_use]
    pub fn start(self) -> AutoScanHandle {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let pool = self.pool;
        let cover_art_dir = self.cover_art_dir;
        let interval = self.interval;
        let scan_state = self.scan_state;

        tokio::spawn(async move {
            Self::run_scan_loop(pool, cover_art_dir, interval, scan_state, shutdown_rx).await;
        });

        AutoScanHandle { shutdown_tx }
    }

    /// Run the scan loop.
    async fn run_scan_loop(
        pool: DbPool,
        cover_art_dir: PathBuf,
        interval: Duration,
        scan_state: ScanStateHandle,
        mut shutdown_rx: watch::Receiver<bool>,
    ) {
        tracing::event!(
            name: "scan.auto.started",
            tracing::Level::INFO,
            scan.interval = ?interval,
            "auto-scanner started"
        );

        loop {
            // Wait for the interval or shutdown signal
            tokio::select! {
                () = tokio::time::sleep(interval) => {
                    // Time to scan
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::event!(
                            name: "scan.auto.shutdown_signal",
                            tracing::Level::INFO,
                            "auto-scanner received shutdown signal"
                        );
                        break;
                    }
                }
            }

            // Run the scan in a blocking task since it uses diesel
            let pool_clone = pool.clone();
            let cover_art_dir_clone = cover_art_dir.clone();
            let scan_state_clone = scan_state.clone();
            let Some(guard) = scan_state_clone.try_start() else {
                tracing::event!(
                    name: "scan.auto.skipped_in_progress",
                    tracing::Level::DEBUG,
                    "auto-scan skipped because a scan is already running"
                );
                continue;
            };
            let result = tokio::task::spawn_blocking(move || {
                let scanner = Scanner::with_cover_art_dir(pool_clone, cover_art_dir_clone);
                let _guard = guard;
                tracing::event!(
                    name: "scan.auto.started_cycle",
                    tracing::Level::DEBUG,
                    scan.mode = "incremental",
                    "auto-scan cycle started"
                );
                scanner
                    .scan_all_with_options(Some(&scan_state_clone), ScanMode::Incremental)
                    .map(AutoScanOutcome::Completed)
            })
            .await;

            match result {
                Ok(Ok(AutoScanOutcome::Completed(stats))) => {
                    tracing::event!(
                        name: "scan.auto.completed",
                        tracing::Level::INFO,
                        tracks.found = stats.tracks_found,
                        tracks.added = stats.tracks_added,
                        tracks.updated = stats.tracks_updated,
                        tracks.skipped = stats.tracks_skipped,
                        tracks.removed = stats.tracks_removed,
                        tracks.failed = stats.tracks_failed,
                        "auto-scan completed"
                    );
                }
                Ok(Err(ScanError::NoMusicFolders)) => {
                    tracing::event!(
                        name: "scan.auto.skipped_no_folders",
                        tracing::Level::DEBUG,
                        "auto-scan skipped because no music folders are configured"
                    );
                }
                Ok(Err(e)) => {
                    tracing::event!(
                        name: "scan.auto.failed",
                        tracing::Level::ERROR,
                        error = %e,
                        "auto-scan failed"
                    );
                }
                Err(e) => {
                    tracing::event!(
                        name: "scan.auto.task_panic",
                        tracing::Level::ERROR,
                        error = %e,
                        "auto-scan task panicked"
                    );
                }
            }
        }

        tracing::event!(
            name: "scan.auto.stopped",
            tracing::Level::INFO,
            "auto-scanner stopped"
        );
    }
}

/// Handle for controlling the auto-scanner.
#[derive(Debug)]
pub struct AutoScanHandle {
    shutdown_tx: watch::Sender<bool>,
}

impl AutoScanHandle {
    /// Stop the auto-scanner.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

impl ExistingFile {
    fn add_leaf(&mut self, leaf: String) {
        if let ExistingFileType::MasterFile { ref mut leafs } = self.r#type {
            leafs.insert(leaf);
        } else {
            self.r#type = ExistingFileType::MasterFile {
                leafs: (leaf,).collect(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::{DiscoveredFile, ExistingFile, ScanMode, Scanner};

    fn discovered(path: &str, file_modified_at: Option<i64>) -> DiscoveredFile {
        DiscoveredFile {
            path: PathBuf::from(path),
            file_size: 123,
            file_modified_at,
        }
    }

    #[test]
    fn split_discovered_files_skips_unchanged_in_incremental_mode() {
        let discovered_files = vec![
            discovered("/music/unchanged.mp3", Some(100)),
            discovered("/music/changed.mp3", Some(200)),
            discovered("/music/new.mp3", Some(300)),
        ];

        let existing = HashMap::from([
            (
                "/music/unchanged.mp3".to_string(),
                ExistingFile {
                    modified_at: Some(100),
                    size: 123,
                    album_id: None,
                },
            ),
            (
                "/music/changed.mp3".to_string(),
                ExistingFile {
                    modified_at: Some(150),
                    size: 123,
                    album_id: None,
                },
            ),
        ]);

        let (to_process, skipped) = Scanner::split_discovered_files_for_mode(
            discovered_files,
            &existing,
            ScanMode::Incremental,
        );

        assert_eq!(skipped, 1);
        assert_eq!(to_process.len(), 2);
        assert!(
            to_process
                .iter()
                .any(|f| f.path.to_string_lossy() == "/music/changed.mp3")
        );
        assert!(
            to_process
                .iter()
                .any(|f| f.path.to_string_lossy() == "/music/new.mp3")
        );
    }

    #[test]
    fn split_discovered_files_keeps_all_in_full_mode() {
        let discovered_files = vec![
            discovered("/music/one.mp3", Some(10)),
            discovered("/music/two.mp3", Some(20)),
        ];

        let existing = HashMap::from([(
            "/music/one.mp3".to_string(),
            ExistingFile {
                modified_at: Some(10),
                size: 123,
                album_id: None,
            },
        )]);

        let (to_process, skipped) =
            Scanner::split_discovered_files_for_mode(discovered_files, &existing, ScanMode::Full);

        assert_eq!(skipped, 0);
        assert_eq!(to_process.len(), 2);
    }

    #[test]
    fn file_modified_at_saturates_u64_overflow_to_i64_max() {
        // When a file has a modification time that doesn't fit in i64,
        // the saturating conversion must yield i64::MAX instead of 0.
        let huge_secs = u64::MAX;
        let converted = i64::try_from(huge_secs).unwrap_or(i64::MAX);
        assert_eq!(converted, i64::MAX);
    }

    #[test]
    fn file_modified_at_preserves_valid_seconds() {
        let normal_secs: u64 = 1_700_000_000;
        let converted = i64::try_from(normal_secs).unwrap_or(i64::MAX);
        assert_eq!(converted, 1_700_000_000_i64);
    }

    #[test]
    fn image_mime_from_ext_maps_supported_cover_extensions() {
        assert_eq!(Scanner::image_mime_from_ext("png"), "image/png");
        assert_eq!(Scanner::image_mime_from_ext("gif"), "image/gif");
        assert_eq!(Scanner::image_mime_from_ext("bmp"), "image/bmp");
        assert_eq!(Scanner::image_mime_from_ext("webp"), "image/webp");
    }

    #[test]
    fn image_mime_from_ext_defaults_unknown_and_jpeg_to_jpeg() {
        assert_eq!(Scanner::image_mime_from_ext("jpg"), "image/jpeg");
        assert_eq!(Scanner::image_mime_from_ext("jpeg"), "image/jpeg");
        assert_eq!(Scanner::image_mime_from_ext("tiff"), "image/jpeg");
        assert_eq!(Scanner::image_mime_from_ext("unknown"), "image/jpeg");
    }
}
