//! Database module for `SQLite` persistence.

pub mod connection;
pub mod repo;
pub(crate) mod schema;

#[doc(inline)]
pub use connection::{DbConfig, DbConn, DbPool, DbPoolError, initialize_database};
#[doc(inline)]
pub use repo::{
    AlbumRepository, ArtistInfoCacheRepository, ArtistRepository, BookmarkEntry,
    BookmarkRepository, InternetRadioRepository, InternetRadioStation, MusicFolderRepository,
    MusicRepoError, MusicRepoErrorKind, NewUser, NowPlayingEntry, NowPlayingRepository, PlayQueue,
    PlayQueueRepository, Playlist, PlaylistRepository, RatingRepository, RemoteCommand,
    RemoteControlRepository, RemoteSession, RemoteState, SETTING_LAST_SCAN_AT,
    SETTING_LASTFM_API_KEY, SETTING_LASTFM_API_SECRET, ScrobbleRepository, SettingsRepository,
    SongRepository, StarredRepository, UserRepoError, UserRepoErrorKind, UserRepository,
    UserUpdate,
};
