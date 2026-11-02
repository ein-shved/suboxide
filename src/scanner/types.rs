//! Scanner types and constants.

use std::path::PathBuf;
use thiserror::Error;

use crate::db::MusicRepoError;

/// Errors that can occur during scanning.
#[derive(Debug, Error)]
pub enum ScanError {
    #[error("Database error: {0}")]
    Database(#[from] MusicRepoError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("No music folders configured")]
    NoMusicFolders,

    #[error("Music folder not found: {0}")]
    FolderNotFound(String),

    #[error("Invalid scan target: {0}")]
    InvalidTarget(String),

    #[error("Scan already in progress")]
    AlreadyScanning,

    #[error("Cleanup failed: {0}")]
    Cleanup(String),

    #[error("Failed to read metadata for {path}: {message}")]
    Metadata { path: PathBuf, message: String },
}

impl From<diesel::result::Error> for ScanError {
    fn from(error: diesel::result::Error) -> Self {
        Self::Database(error.into())
    }
}

/// Supported audio file extensions.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "opus", "m4a", "aac", "wav", "wma", "aiff", "ape", "wv",
];

/// Common cover art filenames to look for in album directories.
/// These are tried in order of preference.
pub const COVER_ART_FILENAMES: &[&str] = &[
    "cover",
    "folder",
    "front",
    "album",
    "albumart",
    "albumartsmall",
    "thumb",
    "art",
];

/// Supported image file extensions for external cover art.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "webp"];

/// Batch size for song inserts (`SQLite` has a limit of ~999 variables per query)
pub const BATCH_SIZE: usize = 100;

/// Default auto-scan interval (5 minutes).
pub const DEFAULT_AUTO_SCAN_INTERVAL_SECS: u64 = 300;

/// Scan mode controlling how files are scanned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanMode {
    /// Full scan - re-scan all files regardless of modification time.
    Full,
    /// Incremental scan - only scan new or modified files.
    #[default]
    Incremental,
}

/// A recursive root selection or one directory's direct entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanTarget {
    AllEnabled,
    Root(i32),
    Directory { root_id: i32, path: PathBuf },
}

/// Metadata extracted from an audio file.
#[derive(Debug, Clone)]
pub struct ScannedTrack {
    pub path: PathBuf,
    pub cue_track: Option<crate::models::music::CueTrackSource>,
    pub parent_path: PathBuf,
    pub file_size: u64,
    pub content_type: String,
    pub suffix: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub year: Option<u32>,
    pub genre: Option<String>,
    pub duration_secs: u32,
    pub bit_rate: Option<u32>,
    pub bit_depth: Option<u8>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u8>,
    /// File modification time (Unix timestamp in seconds).
    pub file_modified_at: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct PreparedTrack {
    pub track: ScannedTrack,
    pub path_str: String,
    pub artist_id: Option<i32>,
    pub album_id: Option<i32>,
    pub cover_art: Option<String>,
    pub identity: crate::models::music::SongIdentity,
    pub song_id: Option<i32>,
}

/// Result of scanning a music folder.
#[derive(Debug, Default)]
pub struct ScanResult {
    pub tracks_found: usize,
    pub tracks_added: usize,
    pub tracks_updated: usize,
    pub tracks_skipped: usize,
    pub tracks_removed: usize,
    pub tracks_failed: usize,
    pub artists_added: usize,
    pub albums_added: usize,
    pub cover_art_saved: usize,
}

impl std::ops::AddAssign for ScanResult {
    fn add_assign(&mut self, other: Self) {
        self.tracks_found += other.tracks_found;
        self.tracks_added += other.tracks_added;
        self.tracks_updated += other.tracks_updated;
        self.tracks_skipped += other.tracks_skipped;
        self.tracks_removed += other.tracks_removed;
        self.tracks_failed += other.tracks_failed;
        self.artists_added += other.artists_added;
        self.albums_added += other.albums_added;
        self.cover_art_saved += other.cover_art_saved;
    }
}
