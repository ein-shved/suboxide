//! Scanner engine logic.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::tag::{Accessor, ItemKey};
use rayon::prelude::*;
use tokio::sync::watch;

use crate::cover_art::{content_hash, extension_from_mime, mime_from_extension};
use crate::db::{DbPool, MusicFolderRepository, MusicRepoError};
use crate::models::music::{MusicFolder, normalize_search_text};
use crate::paths::resolve_cover_art_dir;
use crate::scanner::state::{ScanPhase, ScanStateHandle};
use crate::scanner::types::{
    AUDIO_EXTENSIONS, BATCH_SIZE, COVER_ART_FILENAMES, DEFAULT_AUTO_SCAN_INTERVAL_SECS,
    IMAGE_EXTENSIONS, PreparedTrack, ScanError, ScanMode, ScanResult, ScanTarget, ScannedTrack,
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
}

#[derive(Debug, Clone, Copy)]
struct ExistingSong {
    file_modified_at: Option<i64>,
    file_size: i64,
    album_id: Option<i32>,
}

#[derive(Debug, Default)]
struct DirectoryListing {
    files: Vec<DiscoveredFile>,
    audio_paths: HashSet<String>,
    entries: HashSet<PathBuf>,
    children: Vec<PathBuf>,
    complete: bool,
    failures: usize,
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
        let hash = content_hash(data);
        let extension = extension_from_mime(mime);
        let filepath = self.cover_art_dir.join(format!("{hash}.{extension}"));

        // Only write if file doesn't already exist (same content = same hash)
        if !filepath.exists() {
            fs::write(&filepath, data)?;
        }

        // Return just the hash as the cover art ID
        Ok(hash)
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
                    return Some((data, mime_from_extension(ext).to_string()));
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
                    return Some((data, mime_from_extension(&extension).to_string()));
                }
            }
        }

        None
    }

    /// Normalize and validate a selection before starting background work.
    pub fn resolve_target(&self, target: ScanTarget) -> Result<ScanTarget, ScanError> {
        let folders = MusicFolderRepository::new(self.pool.clone());
        match target {
            ScanTarget::AllEnabled => {
                if folders.find_enabled()?.is_empty() {
                    return Err(ScanError::NoMusicFolders);
                }
                Ok(ScanTarget::AllEnabled)
            }
            ScanTarget::Root(id) => {
                self.selected_root(id)?;
                Ok(ScanTarget::Root(id))
            }
            ScanTarget::Directory { root_id, path } => {
                let root = self.selected_root(root_id)?;
                let relative = crate::db::repo::music::normalize_directory_path(&path)
                    .map_err(|error| ScanError::InvalidTarget(error.to_string()))?;
                let mut current = PathBuf::from(&root.path);
                Self::validate_component(&current)?;
                for component in Path::new(&relative).components() {
                    current.push(component);
                    Self::validate_component(&current)?;
                }
                Ok(ScanTarget::Directory {
                    root_id,
                    path: relative.into(),
                })
            }
        }
    }

    fn selected_root(&self, id: i32) -> Result<MusicFolder, ScanError> {
        let root = MusicFolderRepository::new(self.pool.clone())
            .find_by_id(id)?
            .filter(|folder| folder.parent.is_none())
            .ok_or_else(|| ScanError::FolderNotFound(id.to_string()))?;
        if !root.enabled {
            return Err(ScanError::InvalidTarget(format!("root {id} is disabled")));
        }
        Ok(root)
    }

    fn validate_component(path: &Path) -> Result<(), ScanError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_symlink() || !metadata.is_dir() => Err(
                ScanError::InvalidTarget(format!("{} is not a plain directory", path.display())),
            ),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Apply the same direct-directory boundary to all scan targets.
    pub fn scan(
        &self,
        target: ScanTarget,
        mode: ScanMode,
        state: Option<&ScanStateHandle>,
    ) -> Result<ScanResult, ScanError> {
        let target = self.resolve_target(target)?;
        if let Some(state) = state {
            state.set_phase(ScanPhase::Discovering);
        }
        let result = match target {
            ScanTarget::Directory { root_id, path } => {
                let root = self.selected_root(root_id)?;
                let absolute = Self::directory_path(&root, &path);
                match Self::discover_entries(&absolute) {
                    Ok(listing) => {
                        let result = self.scan_directory(&root, &path, &listing, mode, state)?;
                        self.prune_directory(&root, &path, &listing)?;
                        result
                    }
                    Err(error)
                        if error.kind() == std::io::ErrorKind::NotFound
                            && !path.as_os_str().is_empty()
                            && Self::proven_absent(&absolute)? =>
                    {
                        self.remove_direct(&root, &path)?
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            recursive => {
                let folders = match recursive {
                    ScanTarget::AllEnabled => {
                        MusicFolderRepository::new(self.pool.clone()).find_enabled()?
                    }
                    ScanTarget::Root(id) => vec![self.selected_root(id)?],
                    ScanTarget::Directory { .. } => unreachable!("directory handled above"),
                };
                let mut result = ScanResult::default();
                for root in folders {
                    if let Some(state) = state {
                        state.set_current_folder(Some(root.name.clone()));
                    }
                    result += self.scan_tree(&root, Path::new(""), mode, state)?;
                }
                result
            }
        };
        if let Some(state) = state {
            state.set_phase(ScanPhase::Cleaning);
        }
        self.cleanup_orphans()?;
        crate::db::SettingsRepository::new(self.pool.clone()).set(
            crate::db::SETTING_LAST_SCAN_AT,
            &chrono::Utc::now().timestamp_millis().to_string(),
        )?;
        Ok(result)
    }

    fn directory_path(root: &MusicFolder, relative: &Path) -> PathBuf {
        if relative.as_os_str().is_empty() {
            PathBuf::from(&root.path)
        } else {
            Path::new(&root.path).join(relative)
        }
    }

    fn scan_tree(
        &self,
        root: &MusicFolder,
        relative: &Path,
        mode: ScanMode,
        state: Option<&ScanStateHandle>,
    ) -> Result<ScanResult, ScanError> {
        let absolute = Self::directory_path(root, relative);
        Self::validate_component(&absolute)?;
        let listing = Self::discover_entries(&absolute)?;
        let mut result = self.scan_directory(root, relative, &listing, mode, state)?;
        for child in &listing.children {
            let child_relative = child
                .strip_prefix(&root.path)
                .map_err(|error| ScanError::InvalidTarget(error.to_string()))?;
            match self.scan_tree(root, child_relative, mode, state) {
                Ok(scanned) => result += scanned,
                Err(error @ (ScanError::Io(_) | ScanError::InvalidTarget(_))) => {
                    tracing::warn!(path = %child.display(), %error, "scan directory failed; preserving indexed branch");
                    result.tracks_failed += 1;
                }
                Err(error) => return Err(error),
            }
        }
        if listing.complete {
            let folders = MusicFolderRepository::new(self.pool.clone());
            if let Some(folder) = folders.find(root.id, &relative.to_string_lossy())? {
                for child in folders.children(folder.id)? {
                    if !listing.entries.contains(Path::new(&child.path)) {
                        result += self.remove_branch(&child)?;
                    }
                }
            }
        }
        self.prune_directory(root, relative, &listing)?;
        Ok(result)
    }

    fn prune_directory(
        &self,
        root: &MusicFolder,
        relative: &Path,
        listing: &DirectoryListing,
    ) -> Result<(), ScanError> {
        if listing.complete && listing.audio_paths.is_empty() {
            let folders = MusicFolderRepository::new(self.pool.clone());
            if let Some(folder) = folders.find(root.id, &relative.to_string_lossy())? {
                folders.prune(folder.id)?;
            }
        }
        Ok(())
    }

    fn proven_absent(path: &Path) -> Result<bool, ScanError> {
        let Some(parent) = path.parent() else {
            return Ok(false);
        };
        let listing = Self::discover_entries(parent)?;
        Ok(listing.complete && !listing.entries.contains(path))
    }

    fn remove_direct(&self, root: &MusicFolder, relative: &Path) -> Result<ScanResult, ScanError> {
        let folders = MusicFolderRepository::new(self.pool.clone());
        let Some(folder) = folders.find(root.id, &relative.to_string_lossy())? else {
            return Ok(ScanResult::default());
        };
        let result = self.remove_owned(&folder)?;
        folders.prune(folder.id)?;
        Ok(result)
    }

    fn remove_branch(&self, folder: &MusicFolder) -> Result<ScanResult, ScanError> {
        let folders = MusicFolderRepository::new(self.pool.clone());
        let mut result = self.remove_owned(folder)?;
        for child in folders.children(folder.id)? {
            result += self.remove_branch(&child)?;
        }
        folders.prune(folder.id)?;
        Ok(result)
    }

    fn remove_owned(&self, folder: &MusicFolder) -> Result<ScanResult, ScanError> {
        let existing = self.get_existing_songs(folder.id)?;
        let paths: Vec<_> = existing.keys().cloned().collect();
        let albums = existing.values().filter_map(|song| song.album_id).collect();
        tracing::debug!(name: "scan.directory.reconcile", owner = folder.id, path = %folder.path);
        let removed = self.remove_deleted_songs(&paths)?;
        self.update_album_stats_for_ids(&albums)?;
        Ok(ScanResult {
            tracks_removed: removed,
            ..ScanResult::default()
        })
    }

    fn scan_directory(
        &self,
        root: &MusicFolder,
        relative: &Path,
        listing: &DirectoryListing,
        mode: ScanMode,
        state: Option<&ScanStateHandle>,
    ) -> Result<ScanResult, ScanError> {
        let folders = MusicFolderRepository::new(self.pool.clone());
        let relative = relative.to_string_lossy();
        let folder = if listing.audio_paths.is_empty() {
            folders.find(root.id, &relative)?
        } else {
            Some(folders.ensure(root.id, &relative)?)
        };
        let existing = match &folder {
            Some(folder) => self.get_existing_songs(folder.id)?,
            None => HashMap::new(),
        };
        let (files, skipped) =
            Self::split_discovered_files_for_mode(&listing.files, &existing, mode);
        if let Some(state) = state {
            state.set_phase(ScanPhase::Processing);
            state.set_total(state.get_total() + listing.audio_paths.len() as u64);
            state.set_count(state.get_count() + skipped as u64);
        }
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let tracks: Vec<_> = files.par_iter().filter_map(|file| {
            tracing::dispatcher::with_default(&dispatch, || {
                match Self::read_track_metadata_static(file) {
                    Ok(track) => Some(track),
                    Err(error) => { tracing::warn!(path = %file.path.display(), %error, "scan metadata read failed"); None }
                }
            })
        }).collect();
        let metadata_failures = files.len() - tracks.len();
        let mut result = if let Some(folder) = &folder {
            self.process_tracks_with_options(folder, tracks, &existing, state, skipped)?
        } else {
            ScanResult::default()
        };
        result.tracks_found = listing.audio_paths.len();
        result.tracks_failed += listing.failures + metadata_failures;
        if let Some(folder) = &folder {
            tracing::debug!(name: "scan.directory.reconcile", owner = folder.id, path = %folder.path);
            if listing.complete {
                let deleted: Vec<_> = existing
                    .keys()
                    .filter(|path| !listing.audio_paths.contains(*path))
                    .cloned()
                    .collect();
                let albums = deleted
                    .iter()
                    .filter_map(|path| existing.get(path).and_then(|song| song.album_id))
                    .collect();
                result.tracks_removed += self.remove_deleted_songs(&deleted)?;
                self.update_album_stats_for_ids(&albums)?;
            }
        }
        Ok(result)
    }

    /// Get existing songs in a folder from the database.
    fn get_existing_songs(
        &self,
        folder_id: i32,
    ) -> Result<HashMap<String, ExistingSong>, ScanError> {
        use crate::db::schema::songs;
        use diesel::prelude::*;

        let mut conn = self.pool.get().map_err(MusicRepoError::from)?;
        let existing = songs::table
            .filter(songs::music_folder_id.eq(folder_id))
            .select((
                songs::path,
                songs::file_modified_at,
                songs::file_size,
                songs::album_id,
                songs::music_folder_id,
            ))
            .load::<(String, Option<i64>, i64, Option<i32>, i32)>(&mut conn)
            .map_err(MusicRepoError::from)?;

        tracing::debug!(name: "scan.directory.read", owner = folder_id, count = existing.len());
        Ok(existing
            .into_iter()
            .map(|(path, file_modified_at, file_size, album_id, owner)| {
                tracing::debug!(name: "scan.directory.member", owner = folder_id, returned_owner = owner);
                (
                    path,
                    ExistingSong {
                        file_modified_at,
                        file_size,
                        album_id,
                    },
                )
            })
            .collect())
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

    /// List one directory; incomplete listings never prove absence.
    fn discover_entries(path: &Path) -> Result<DirectoryListing, std::io::Error> {
        tracing::debug!(name: "scan.directory.enumerate", path = %path.display());
        let entries = fs::read_dir(path)?;
        let mut listing = DirectoryListing {
            complete: true,
            ..DirectoryListing::default()
        };
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    listing.entries.insert(path.clone());
                    if let Err(error) = Self::classify_entry(&path, &mut listing) {
                        listing.complete = false;
                        listing.failures += 1;
                        tracing::warn!(path = %path.display(), %error, "scan entry classification failed");
                    }
                }
                Err(error) => {
                    listing.complete = false;
                    listing.failures += 1;
                    tracing::warn!(path = %path.display(), %error, "scan directory listing failed");
                }
            }
        }
        Ok(listing)
    }

    fn classify_entry(path: &Path, listing: &mut DirectoryListing) -> Result<(), std::io::Error> {
        tracing::debug!(name: "scan.entry.classify", path = %path.display());
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            listing.children.push(path.to_path_buf());
            return Ok(());
        }
        if !path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                AUDIO_EXTENSIONS
                    .iter()
                    .any(|supported| ext.eq_ignore_ascii_case(supported))
            })
        {
            return Ok(());
        }
        if metadata.is_symlink() && fs::metadata(path)?.is_dir() {
            return Ok(());
        }
        if !metadata.is_file() && !metadata.is_symlink() {
            return Ok(());
        }
        listing
            .audio_paths
            .insert(path.to_string_lossy().into_owned());
        tracing::debug!(name: "scan.track.stat", path = %path.display());
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                listing.failures += 1;
                tracing::warn!(path = %path.display(), %error, "scan file stat failed");
                return Ok(());
            }
        };
        let file_modified_at = match metadata.modified() {
            Ok(time) => time
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)),
            Err(error) => {
                listing.failures += 1;
                tracing::warn!(path = %path.display(), %error, "scan file modification time failed");
                None
            }
        };
        listing.files.push(DiscoveredFile {
            path: path.to_path_buf(),
            file_size: metadata.len(),
            file_modified_at,
        });
        Ok(())
    }

    fn split_discovered_files_for_mode<'a>(
        discovered_files: &'a [DiscoveredFile],
        existing_songs: &HashMap<String, ExistingSong>,
        mode: ScanMode,
    ) -> (Vec<&'a DiscoveredFile>, usize) {
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
                    matches!((stored.file_modified_at, file.file_modified_at), (Some(stored), Some(current)) if stored == current)
                        && stored.file_size == i64::try_from(file.file_size).unwrap_or(i64::MAX)
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
    fn read_track_metadata_static(file: &DiscoveredFile) -> Result<ScannedTrack, ScanError> {
        let path = &file.path;
        tracing::debug!(name: "scan.track.open", path = %path.display());
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .unwrap_or_default();

        // Persist the absolute containing directory.
        let parent_path = path.parent().map(Path::to_path_buf).unwrap_or_default();

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
        existing_songs: &HashMap<String, ExistingSong>,
        state: Option<&ScanStateHandle>,
        initial_tracks_skipped: usize,
    ) -> Result<ScanResult, ScanError> {
        use crate::db::schema::{albums, artists, songs};
        use diesel::prelude::*;

        if tracks.is_empty() {
            return Ok(ScanResult {
                tracks_skipped: initial_tracks_skipped,
                ..ScanResult::default()
            });
        }

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
                        .values((
                            artists::name.eq(name),
                            artists::search_name.eq(normalize_search_text(name)),
                        ))
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
                            albums::search_name.eq(normalize_search_text(album_name)),
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
                                    tracing::event!(
                                        name: "scan.cover_art.album_update_failed",
                                        tracing::Level::WARN,
                                        album.id = album_id,
                                        error = %e,
                                        "failed to update album cover art"
                                    );
                                    None
                                } else {
                                    album_cover_art_cache
                                        .insert(album_id, Some(cover_art_hash.clone()));
                                    result.cover_art_saved += 1;
                                    Some(cover_art_hash)
                                }
                            }
                            Err(e) => {
                                tracing::event!(
                                    name: "scan.cover_art.save_failed",
                                    tracing::Level::WARN,
                                    error = %e,
                                    "failed to save cover art"
                                );
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
                    let search_name = normalize_search_text(&prepared.track.title);
                    let query_result =
                        if prepared.is_update {
                            diesel::update(songs::table.filter(songs::path.eq(&prepared.path_str)))
                                .set((
                                    songs::title.eq(&prepared.track.title),
                                    songs::search_name.eq(&search_name),
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
                                    songs::search_name.eq(&search_name),
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
                if let Some(album_id) = existing_songs
                    .get(&prepared.path_str)
                    .and_then(|song| song.album_id)
                {
                    dirty_album_ids.insert(album_id);
                }
                if let Some(album_id) = prepared.album_id {
                    dirty_album_ids.insert(album_id);
                }
                if let Some(state) = state {
                    state.set_count(state.get_count() + 1);
                }
            }
        }

        // Update album song counts and durations for dirty albums only
        crate::db::repo::music::reconcile_albums(&mut conn, &dirty_album_ids)
            .map_err(MusicRepoError::from)?;

        Ok(result)
    }

    fn update_album_stats_for_ids(&self, dirty_album_ids: &HashSet<i32>) -> Result<(), ScanError> {
        let mut conn = self.pool.get().map_err(MusicRepoError::from)?;
        crate::db::repo::music::reconcile_albums(&mut conn, dirty_album_ids)
            .map_err(MusicRepoError::from)?;
        Ok(())
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
                    .scan(
                        ScanTarget::AllEnabled,
                        ScanMode::Incremental,
                        Some(&scan_state_clone),
                    )
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::{DiscoveredFile, ExistingSong, ScanMode, Scanner};

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
                ExistingSong {
                    file_modified_at: Some(100),
                    file_size: 123,
                    album_id: None,
                },
            ),
            (
                "/music/changed.mp3".to_string(),
                ExistingSong {
                    file_modified_at: Some(150),
                    file_size: 123,
                    album_id: None,
                },
            ),
        ]);

        let (to_process, skipped) = Scanner::split_discovered_files_for_mode(
            &discovered_files,
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
            ExistingSong {
                file_modified_at: Some(10),
                file_size: 123,
                album_id: None,
            },
        )]);

        let (to_process, skipped) =
            Scanner::split_discovered_files_for_mode(&discovered_files, &existing, ScanMode::Full);

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
}
