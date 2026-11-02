//! Database connection pool and management.

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, CustomizeConnection, Pool, PooledConnection};
use std::time::Duration;
use thiserror::Error;

/// Type alias for our connection pool.
pub type DbPool = Pool<ConnectionManager<SqliteConnection>>;

/// Type alias for a pooled connection.
pub type DbConn = PooledConnection<ConnectionManager<SqliteConnection>>;

/// Error returned when building a database pool fails.
#[derive(Debug, Error)]
#[error("failed to build database pool: {message}")]
pub struct DbPoolError {
    message: String,
    #[source]
    source: Option<diesel::r2d2::PoolError>,
}

impl DbPoolError {
    /// Returns the underlying pool error.
    #[must_use]
    pub const fn source_error(&self) -> Option<&diesel::r2d2::PoolError> {
        self.source.as_ref()
    }

    fn config(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    fn pool(source: diesel::r2d2::PoolError) -> Self {
        Self {
            message: source.to_string(),
            source: Some(source),
        }
    }
}

/// Database configuration.
#[derive(Debug, Clone)]
pub struct DbConfig {
    /// Path to the `SQLite` database file.
    pub database_url: String,
    /// Maximum number of connections in the pool.
    pub max_connections: u32,
    /// Connection timeout in seconds.
    pub connection_timeout: u64,
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            database_url: "subsonic.db".to_string(),
            max_connections: 10,
            connection_timeout: 30,
        }
    }
}

impl DbConfig {
    /// Create a new database configuration.
    pub fn new(database_url: impl Into<String>) -> Self {
        Self {
            database_url: database_url.into(),
            ..Default::default()
        }
    }

    /// Build a connection pool from this configuration.
    pub fn build_pool(&self) -> Result<DbPool, DbPoolError> {
        if self.max_connections == 0 {
            return Err(DbPoolError::config("max_connections must be positive"));
        }

        let manager = ConnectionManager::<SqliteConnection>::new(&self.database_url);

        Pool::builder()
            .max_size(self.max_connections)
            .connection_timeout(Duration::from_secs(self.connection_timeout))
            .connection_customizer(Box::new(SqliteConnectionCustomizer))
            .build(manager)
            .map_err(DbPoolError::pool)
    }
}

/// Customizer that applies `SQLite` PRAGMAs to each new connection.
/// This ensures all pooled connections have consistent settings.
#[derive(Debug)]
struct SqliteConnectionCustomizer;

impl CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for SqliteConnectionCustomizer {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
        // Set busy timeout to 5 seconds - SQLite will retry on lock contention
        // instead of immediately returning SQLITE_BUSY
        diesel::sql_query("PRAGMA busy_timeout = 5000")
            .execute(conn)
            .map_err(diesel::r2d2::Error::QueryError)?;

        // Enable foreign keys (not inherited across connections)
        diesel::sql_query("PRAGMA foreign_keys = ON")
            .execute(conn)
            .map_err(diesel::r2d2::Error::QueryError)?;

        // Set synchronous to NORMAL for better write performance while still being safe
        diesel::sql_query("PRAGMA synchronous = NORMAL")
            .execute(conn)
            .map_err(diesel::r2d2::Error::QueryError)?;

        // Increase cache size for better read performance (negative = KB, so -64000 = 64MB)
        diesel::sql_query("PRAGMA cache_size = -64000")
            .execute(conn)
            .map_err(diesel::r2d2::Error::QueryError)?;

        // Enable memory-mapped I/O for faster reads (256MB)
        diesel::sql_query("PRAGMA mmap_size = 268435456")
            .execute(conn)
            .map_err(diesel::r2d2::Error::QueryError)?;

        Ok(())
    }
}

/// Create the current database schema and indexes if they do not exist.
///
/// Upgrade the previous song schema in place, preserving IDs and references.
pub fn initialize_database(conn: &mut SqliteConnection) -> Result<(), diesel::result::Error> {
    // SQLite table rebuilding requires foreign keys disabled outside the
    // transaction. Restore the caller's setting on both success and failure.
    #[derive(QueryableByName)]
    struct ForeignKeys {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        foreign_keys: i32,
    }
    diesel::sql_query("PRAGMA journal_mode = WAL").execute(conn)?;
    let enabled = diesel::sql_query("PRAGMA foreign_keys")
        .get_result::<ForeignKeys>(conn)?
        .foreign_keys
        != 0;
    diesel::sql_query("PRAGMA foreign_keys = OFF").execute(conn)?;
    let result = conn.transaction(initialize_schema);
    if enabled {
        diesel::sql_query("PRAGMA foreign_keys = ON").execute(conn)?;
    }
    result
}

fn upgrade_cue_songs(conn: &mut SqliteConnection) -> Result<(), diesel::result::Error> {
    #[derive(QueryableByName)]
    struct Column {
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
    }
    let columns =
        diesel::sql_query("SELECT name FROM pragma_table_info('songs')").load::<Column>(conn)?;
    if columns.iter().any(|column| column.name == "cue_track_id") {
        return Ok(());
    }
    // Reuse the current table definition; copy every old column explicitly so
    // the added nullable CUE identity receives NULL for ordinary songs.
    let column_names = columns
        .iter()
        .map(|column| format!("\"{}\"", column.name.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(", ");
    diesel::sql_query(SONGS_TABLE.replace("IF NOT EXISTS songs", "songs_cue_upgrade"))
        .execute(conn)?;
    diesel::sql_query(format!(
        "INSERT INTO songs_cue_upgrade ({column_names}) SELECT {column_names} FROM songs"
    ))
    .execute(conn)?;
    diesel::sql_query("DROP TABLE songs").execute(conn)?;
    diesel::sql_query("ALTER TABLE songs_cue_upgrade RENAME TO songs").execute(conn)?;
    Ok(())
}

const SONGS_TABLE: &str = r"
        CREATE TABLE IF NOT EXISTS songs (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            title TEXT NOT NULL,
            sort_name TEXT,
            album_id INTEGER REFERENCES albums(id),
            artist_id INTEGER REFERENCES artists(id),
            artist_name TEXT,
            album_name TEXT,
            music_folder_id INTEGER NOT NULL REFERENCES music_folders(id),
            path TEXT NOT NULL,
            cue_track_id INTEGER UNIQUE REFERENCES cue_tracks(id) DEFERRABLE INITIALLY DEFERRED,
            parent_path TEXT NOT NULL,
            file_size BIGINT NOT NULL DEFAULT 0,
            content_type TEXT NOT NULL,
            suffix TEXT NOT NULL,
            duration INTEGER NOT NULL DEFAULT 0,
            bit_rate INTEGER,
            bit_depth INTEGER,
            sampling_rate INTEGER,
            channel_count INTEGER,
            track_number INTEGER,
            disc_number INTEGER,
            year INTEGER,
            genre TEXT,
            cover_art TEXT,
            musicbrainz_id TEXT,
            search_name TEXT NOT NULL DEFAULT '',
            play_count INTEGER NOT NULL DEFAULT 0,
            file_modified_at BIGINT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )";

#[expect(
    clippy::too_many_lines,
    reason = "Existing schema bootstrap is centralized"
)]
fn initialize_schema(conn: &mut SqliteConnection) -> Result<(), diesel::result::Error> {
    // Other PRAGMAs (busy_timeout, synchronous, cache_size, foreign_keys, mmap_size)
    // are set per-connection via SqliteConnectionCustomizer in build_pool().

    // Create users table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            email TEXT,
            admin_role BOOLEAN NOT NULL DEFAULT FALSE,
            settings_role BOOLEAN NOT NULL DEFAULT TRUE,
            stream_role BOOLEAN NOT NULL DEFAULT TRUE,
            jukebox_role BOOLEAN NOT NULL DEFAULT FALSE,
            download_role BOOLEAN NOT NULL DEFAULT TRUE,
            upload_role BOOLEAN NOT NULL DEFAULT FALSE,
            playlist_role BOOLEAN NOT NULL DEFAULT TRUE,
            cover_art_role BOOLEAN NOT NULL DEFAULT TRUE,
            comment_role BOOLEAN NOT NULL DEFAULT FALSE,
            podcast_role BOOLEAN NOT NULL DEFAULT FALSE,
            share_role BOOLEAN NOT NULL DEFAULT FALSE,
            video_conversion_role BOOLEAN NOT NULL DEFAULT FALSE,
            max_bit_rate INTEGER NOT NULL DEFAULT 0,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            subsonic_password TEXT,
            api_key TEXT,
            lastfm_session_key TEXT
        )
        ",
    )
    .execute(conn)?;

    // Create index for username lookups
    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_users_username ON users(username)")
        .execute(conn)?;

    // Create unique index for API key lookups (only for non-null values)
    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_api_key ON users(api_key) WHERE api_key IS NOT NULL"
    )
    .execute(conn)?;

    // Create index for Last.fm session key lookups
    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_users_lastfm_session ON users(lastfm_session_key) WHERE lastfm_session_key IS NOT NULL"
    )
    .execute(conn)?;

    // Create music_folders table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS music_folders (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            parent INTEGER REFERENCES music_folders(id) ON DELETE CASCADE,
            enabled BOOLEAN NOT NULL DEFAULT TRUE,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_music_folders_parent ON music_folders(parent)",
    )
    .execute(conn)?;

    // Create artists table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS artists (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            name TEXT NOT NULL,
            sort_name TEXT,
            musicbrainz_id TEXT,
            cover_art TEXT,
            artist_image_url TEXT,
            search_name TEXT NOT NULL DEFAULT '',
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_artists_name ON artists(name)")
        .execute(conn)?;

    // Create albums table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS albums (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            name TEXT NOT NULL,
            sort_name TEXT,
            artist_id INTEGER REFERENCES artists(id),
            artist_name TEXT,
            year INTEGER,
            genre TEXT,
            cover_art TEXT,
            musicbrainz_id TEXT,
            search_name TEXT NOT NULL DEFAULT '',
            duration INTEGER NOT NULL DEFAULT 0,
            song_count INTEGER NOT NULL DEFAULT 0,
            play_count INTEGER NOT NULL DEFAULT 0,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_albums_name ON albums(name)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_albums_artist_id ON albums(artist_id)")
        .execute(conn)?;

    // Create or upgrade songs before recreating their indexes.
    diesel::sql_query("CREATE TABLE IF NOT EXISTS cue_sources (
        id INTEGER PRIMARY KEY NOT NULL,
        sheet_path TEXT NOT NULL UNIQUE,
        cue_content TEXT NOT NULL,
        image_size BIGINT NOT NULL CHECK (image_size > 0),
        image_modified_seconds BIGINT NOT NULL CHECK (image_modified_seconds >= 0),
        image_modified_nanos INTEGER NOT NULL CHECK (image_modified_nanos >= 0 AND image_modified_nanos < 1000000000)
    )").execute(conn)?;
    diesel::sql_query(
        "CREATE TABLE IF NOT EXISTS cue_tracks (
        id INTEGER PRIMARY KEY NOT NULL,
        cue_source_id INTEGER NOT NULL REFERENCES cue_sources(id) DEFERRABLE INITIALLY DEFERRED,
        track_number INTEGER NOT NULL CHECK (track_number > 0),
        start_frame BIGINT NOT NULL CHECK (start_frame >= 0),
        end_frame BIGINT NOT NULL CHECK (end_frame > start_frame),
        byte_start BIGINT NOT NULL CHECK (byte_start >= 0),
        byte_end BIGINT NOT NULL CHECK (byte_end > byte_start),
        UNIQUE (cue_source_id, track_number)
    )",
    )
    .execute(conn)?;
    diesel::sql_query(SONGS_TABLE).execute(conn)?;
    upgrade_cue_songs(conn)?;
    diesel::sql_query("CREATE UNIQUE INDEX IF NOT EXISTS idx_songs_file_identity ON songs(path) WHERE cue_track_id IS NULL").execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_songs_title ON songs(title)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_songs_album_id ON songs(album_id)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_songs_artist_id ON songs(artist_id)")
        .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_songs_music_folder_id ON songs(music_folder_id)",
    )
    .execute(conn)?;

    // Additional indexes for common query patterns
    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_songs_genre ON songs(genre)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_songs_year ON songs(year)").execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_songs_artist_name ON songs(artist_name)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_albums_genre ON albums(genre)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_albums_year ON albums(year)")
        .execute(conn)?;

    // Composite index for album queries by artist and year
    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_albums_artist_year ON albums(artist_id, year)",
    )
    .execute(conn)?;

    // Create starred table for favorites
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS starred (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            artist_id INTEGER REFERENCES artists(id) ON DELETE CASCADE,
            album_id INTEGER REFERENCES albums(id) ON DELETE CASCADE,
            song_id INTEGER REFERENCES songs(id) ON DELETE CASCADE,
            starred_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            CHECK (
                (artist_id IS NOT NULL AND album_id IS NULL AND song_id IS NULL) OR
                (artist_id IS NULL AND album_id IS NOT NULL AND song_id IS NULL) OR
                (artist_id IS NULL AND album_id IS NULL AND song_id IS NOT NULL)
            )
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_starred_user_id ON starred(user_id)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_starred_artist_id ON starred(artist_id)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_starred_album_id ON starred(album_id)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_starred_song_id ON starred(song_id)")
        .execute(conn)?;

    // Unique constraint to prevent duplicate stars
    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_starred_user_artist ON starred(user_id, artist_id) WHERE artist_id IS NOT NULL"
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_starred_user_album ON starred(user_id, album_id) WHERE album_id IS NOT NULL"
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_starred_user_song ON starred(user_id, song_id) WHERE song_id IS NOT NULL"
    )
    .execute(conn)?;

    // Create now_playing table for currently playing songs
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS now_playing (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            song_id INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
            player_id TEXT,
            started_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            minutes_ago INTEGER NOT NULL DEFAULT 0
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_now_playing_user_id ON now_playing(user_id)")
        .execute(conn)?;

    // Only one "now playing" entry per user
    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_now_playing_user ON now_playing(user_id)",
    )
    .execute(conn)?;

    // Create scrobbles table for play history
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS scrobbles (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            song_id INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
            played_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            submission BOOLEAN NOT NULL DEFAULT TRUE
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_scrobbles_user_id ON scrobbles(user_id)")
        .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_scrobbles_song_id ON scrobbles(song_id)")
        .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_scrobbles_played_at ON scrobbles(played_at DESC)",
    )
    .execute(conn)?;

    // Create artist_lastfm_info table for cached metadata
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS artist_lastfm_info (
            artist_id INTEGER PRIMARY KEY NOT NULL REFERENCES artists(id) ON DELETE CASCADE,
            biography TEXT,
            last_fm_url TEXT,
            small_image_url TEXT,
            medium_image_url TEXT,
            large_image_url TEXT,
            similar_artists TEXT,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_artist_lastfm_updated ON artist_lastfm_info(updated_at)",
    )
    .execute(conn)?;

    // Create user_ratings table for song/album/artist ratings
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS user_ratings (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            song_id INTEGER REFERENCES songs(id) ON DELETE CASCADE,
            album_id INTEGER REFERENCES albums(id) ON DELETE CASCADE,
            artist_id INTEGER REFERENCES artists(id) ON DELETE CASCADE,
            rating INTEGER NOT NULL CHECK (rating >= 0 AND rating <= 5),
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            CHECK (
                (song_id IS NOT NULL AND album_id IS NULL AND artist_id IS NULL) OR
                (song_id IS NULL AND album_id IS NOT NULL AND artist_id IS NULL) OR
                (song_id IS NULL AND album_id IS NULL AND artist_id IS NOT NULL)
            )
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_user_ratings_user_id ON user_ratings(user_id)",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_user_ratings_user_song ON user_ratings(user_id, song_id) WHERE song_id IS NOT NULL"
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_user_ratings_user_album ON user_ratings(user_id, album_id) WHERE album_id IS NOT NULL"
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_user_ratings_user_artist ON user_ratings(user_id, artist_id) WHERE artist_id IS NOT NULL"
    )
    .execute(conn)?;

    // Create playlists table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS playlists (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            comment TEXT,
            public BOOLEAN NOT NULL DEFAULT FALSE,
            song_count INTEGER NOT NULL DEFAULT 0,
            duration INTEGER NOT NULL DEFAULT 0,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query("CREATE INDEX IF NOT EXISTS idx_playlists_user_id ON playlists(user_id)")
        .execute(conn)?;

    // Create playlist_songs table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS playlist_songs (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            song_id INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_playlist_songs_playlist_id ON playlist_songs(playlist_id)",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_playlist_songs_song_id ON playlist_songs(song_id)",
    )
    .execute(conn)?;

    // Create play_queue table for per-user play queue state
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS play_queue (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            current_song_id INTEGER REFERENCES songs(id) ON DELETE SET NULL,
            position BIGINT,
            changed_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            changed_by TEXT
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_play_queue_user_id ON play_queue(user_id)",
    )
    .execute(conn)?;

    // Create play_queue_songs table for songs in the play queue
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS play_queue_songs (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            play_queue_id INTEGER NOT NULL REFERENCES play_queue(id) ON DELETE CASCADE,
            song_id INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
            position INTEGER NOT NULL
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_play_queue_songs_queue_id ON play_queue_songs(play_queue_id)"
    )
    .execute(conn)?;

    // Create remote control session table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS remote_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            session_id TEXT NOT NULL UNIQUE,
            pairing_code TEXT NOT NULL UNIQUE,
            owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            host_device_id TEXT NOT NULL,
            host_device_name TEXT,
            controller_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
            controller_device_id TEXT,
            controller_device_name TEXT,
            expires_at TIMESTAMP NOT NULL,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            closed_at TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_remote_sessions_owner ON remote_sessions(owner_user_id)",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_remote_sessions_expires ON remote_sessions(expires_at)",
    )
    .execute(conn)?;

    // Create remote command queue table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS remote_commands (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            session_id TEXT NOT NULL REFERENCES remote_sessions(session_id) ON DELETE CASCADE,
            source_device_id TEXT NOT NULL,
            command TEXT NOT NULL,
            payload TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    diesel::sql_query(
        "CREATE INDEX IF NOT EXISTS idx_remote_commands_session_id ON remote_commands(session_id)",
    )
    .execute(conn)?;

    // Create latest remote state table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS remote_state (
            session_id TEXT PRIMARY KEY NOT NULL REFERENCES remote_sessions(session_id) ON DELETE CASCADE,
            state_json TEXT NOT NULL,
            updated_by_device_id TEXT NOT NULL,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    // Create settings table for server-level key-value configuration
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
        )
        ",
    )
    .execute(conn)?;

    // Create bookmarks table for playback positions within songs
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS bookmarks (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            song_id INTEGER NOT NULL REFERENCES songs(id) ON DELETE CASCADE,
            position BIGINT NOT NULL DEFAULT 0,
            comment TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    // One bookmark per user and song
    diesel::sql_query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_bookmarks_user_song ON bookmarks(user_id, song_id)",
    )
    .execute(conn)?;

    // Create internet radio stations table
    diesel::sql_query(
        r"
        CREATE TABLE IF NOT EXISTS internet_radio_stations (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            name TEXT NOT NULL,
            stream_url TEXT NOT NULL,
            home_page_url TEXT,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        ",
    )
    .execute(conn)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use diesel::connection::SimpleConnection;

    fn create_legacy_songs(conn: &mut SqliteConnection) {
        // Build exactly the previous songs schema with the path UNIQUE key.
        conn.batch_execute("PRAGMA foreign_keys = OFF; DROP TABLE songs;")
            .expect("old table setup");
        let old = SONGS_TABLE.replace(
            "            path TEXT NOT NULL,",
            "            path TEXT NOT NULL UNIQUE,",
        );
        let begin = old
            .find("            cue_track_id INTEGER")
            .expect("cue column");
        let end = old
            .find("            parent_path TEXT")
            .expect("next column");
        let old = format!("{}{}", &old[..begin], &old[end..]);
        conn.batch_execute(&old).expect("legacy schema");
        conn.batch_execute("PRAGMA foreign_keys = ON;
            INSERT INTO users (id, username, password_hash) VALUES (1, 'listener', 'hash');
            INSERT INTO music_folders (id, name, path) VALUES (1, 'music', '/music');
            INSERT INTO songs (id, title, music_folder_id, path, parent_path, content_type, suffix, play_count)
              VALUES (42, 'Original', 1, '/music/image.flac', '/music', 'audio/flac', 'flac', 9);
            INSERT INTO playlists (id, user_id, name) VALUES (1, 1, 'Saved');
            INSERT INTO playlist_songs (playlist_id, song_id, position) VALUES (1, 42, 0);
            INSERT INTO play_queue (id, user_id) VALUES (1, 1);
            INSERT INTO play_queue_songs (play_queue_id, song_id, position) VALUES (1, 42, 0);
            INSERT INTO bookmarks (user_id, song_id, position) VALUES (1, 42, 1234);
        ").expect("legacy data");
    }

    #[test]
    fn cue_upgrade_preserves_song_ids_and_personal_references() {
        use crate::db::schema::{bookmarks, play_queue_songs, playlist_songs, songs};
        let mut conn = SqliteConnection::establish(":memory:").expect("connection");
        initialize_database(&mut conn).expect("tables");
        create_legacy_songs(&mut conn);
        initialize_database(&mut conn).expect("upgrade");
        initialize_database(&mut conn).expect("idempotent initialization");
        assert_eq!(
            songs::table
                .find(42)
                .select((songs::title, songs::play_count, songs::cue_track_id))
                .first::<(String, i32, Option<i32>)>(&mut conn)
                .expect("preserved row"),
            ("Original".to_owned(), 9, None)
        );
        assert_eq!(
            playlist_songs::table
                .select(playlist_songs::song_id)
                .first::<i32>(&mut conn)
                .expect("playlist"),
            42
        );
        assert_eq!(
            play_queue_songs::table
                .select(play_queue_songs::song_id)
                .first::<i32>(&mut conn)
                .expect("queue"),
            42
        );
        assert_eq!(
            bookmarks::table
                .select((bookmarks::song_id, bookmarks::position))
                .first::<(i32, i64)>(&mut conn)
                .expect("bookmark"),
            (42, 1234)
        );
        assert!(
            diesel::insert_into(bookmarks::table)
                .values((
                    bookmarks::user_id.eq(1),
                    bookmarks::song_id.eq(999),
                    bookmarks::position.eq(0)
                ))
                .execute(&mut conn)
                .is_err(),
            "foreign key enforcement restored"
        );
    }

    #[test]
    fn cue_relations_enforce_identity_intervals_and_deferred_replacement() {
        let mut conn = SqliteConnection::establish(":memory:").expect("connection");
        initialize_database(&mut conn).expect("tables");
        conn.batch_execute("PRAGMA foreign_keys = ON;
            INSERT INTO cue_sources VALUES (1, '/music/a.cue', 'sheet', 1000, 1, 999999999);
            INSERT INTO cue_tracks VALUES (1, 1, 1, 0, 1, 42, 1000);
            INSERT INTO music_folders (id, name, path) VALUES (1, 'Music', '/music');
            INSERT INTO songs (id, title, music_folder_id, path, parent_path, content_type, suffix, cue_track_id)
                VALUES (1, 'Track', 1, '/music/image.flac', '/music', 'audio/flac', 'flac', 1);
            INSERT INTO songs (id, title, music_folder_id, path, parent_path, content_type, suffix)
                VALUES (2, 'File', 1, '/music/image.flac', '/music', 'audio/flac', 'flac');
        ").expect("typed source and distinct ordinary identity");
        for statement in [
            "INSERT INTO cue_sources VALUES (2, '/music/a.cue', '', 1000, 1, 0)",
            "INSERT INTO cue_sources VALUES (2, '/music/b.cue', '', 0, 1, 0)",
            "INSERT INTO cue_sources VALUES (2, '/music/b.cue', '', 1, -1, 0)",
            "INSERT INTO cue_sources VALUES (2, '/music/b.cue', '', 1, 1, -1)",
            "INSERT INTO cue_sources VALUES (2, '/music/b.cue', '', 1, 1, 1000000000)",
            "INSERT INTO cue_tracks VALUES (2, 1, 1, 1, 2, 100, 200)",
            "INSERT INTO cue_tracks VALUES (2, 1, 0, 1, 2, 100, 200)",
            "INSERT INTO cue_tracks VALUES (2, 1, 2, -1, 2, 100, 200)",
            "INSERT INTO cue_tracks VALUES (2, 1, 2, 1, 1, 100, 200)",
            "INSERT INTO cue_tracks VALUES (2, 1, 2, 1, 2, -1, 200)",
            "INSERT INTO cue_tracks VALUES (2, 1, 2, 1, 2, 100, 100)",
            "INSERT INTO cue_tracks VALUES (2, 999, 2, 1, 2, 100, 200)",
            "UPDATE songs SET cue_track_id = 1 WHERE id = 2",
            "UPDATE songs SET cue_track_id = 999 WHERE id = 2",
            "INSERT INTO songs (title, music_folder_id, path, parent_path, content_type, suffix) VALUES ('Duplicate', 1, '/music/image.flac', '/music', 'audio/flac', 'flac')",
            "DELETE FROM cue_sources WHERE id = 1",
            "DELETE FROM cue_tracks WHERE id = 1",
        ] {
            assert!(
                conn.batch_execute(statement).is_err(),
                "must reject: {statement}"
            );
        }
        conn.transaction::<_, diesel::result::Error, _>(|conn| {
            conn.batch_execute(
                "DELETE FROM cue_sources WHERE id = 1;
                DELETE FROM cue_tracks WHERE id = 1;
                INSERT INTO cue_tracks VALUES (1, 1, 1, 2, 4, 100, 900);
                INSERT INTO cue_sources VALUES (1, '/music/a.cue', 'replacement', 1000, 2, 0);",
            )
        })
        .expect("deferred NO ACTION allows same-ID replacement");
        assert_eq!(
            crate::db::schema::songs::table
                .find(1)
                .select(crate::db::schema::songs::cue_track_id)
                .first::<Option<i32>>(&mut conn)
                .expect("song reference"),
            Some(1)
        );
    }

    #[test]
    fn initialization_creates_current_schema_and_preserves_existing_rows() {
        use crate::db::schema::{music_folders, songs};
        let mut conn = SqliteConnection::establish(":memory:").expect("connection");
        conn.batch_execute("PRAGMA foreign_keys = ON")
            .expect("foreign keys");
        initialize_database(&mut conn).expect("fresh schema");
        conn.batch_execute("
            INSERT INTO music_folders (id, name, path, enabled) VALUES (7, 'offline', '/unavailable-library', FALSE);
            INSERT INTO music_folders (id, name, path, parent) VALUES (8, 'Album', '/unavailable-library/Album', 7);
            INSERT INTO songs (id, title, music_folder_id, path, parent_path, content_type, suffix)
            VALUES (11, 'Track', 8, '/unavailable-library/Album/track.wav', '/unavailable-library/Album', 'audio/wav', 'wav');
        ").expect("current library");
        assert!(
            diesel::sql_query(
                "INSERT INTO music_folders (name, path, parent) VALUES ('invalid', '/invalid', 999)"
            )
            .execute(&mut conn)
            .is_err()
        );
        let folders = music_folders::table
            .select((
                music_folders::id,
                music_folders::path,
                music_folders::parent,
                music_folders::enabled,
            ))
            .load::<(i32, String, Option<i32>, bool)>(&mut conn)
            .expect("folder hierarchy");
        let tracks = songs::table
            .select((
                songs::id,
                songs::music_folder_id,
                songs::path,
                songs::search_name,
            ))
            .load::<(i32, i32, String, String)>(&mut conn)
            .expect("direct owners");
        initialize_database(&mut conn).expect("repeat initialization with offline root");
        assert_eq!(
            music_folders::table
                .select((
                    music_folders::id,
                    music_folders::path,
                    music_folders::parent,
                    music_folders::enabled
                ))
                .load::<(i32, String, Option<i32>, bool)>(&mut conn)
                .expect("unchanged folders"),
            folders
        );
        assert_eq!(
            songs::table
                .select((
                    songs::id,
                    songs::music_folder_id,
                    songs::path,
                    songs::search_name
                ))
                .load::<(i32, i32, String, String)>(&mut conn)
                .expect("unchanged songs"),
            tracks
        );
    }

    #[test]
    fn test_default_config() {
        let config = DbConfig::default();
        assert_eq!(config.database_url, "subsonic.db");
        assert_eq!(config.max_connections, 10);
    }

    #[test]
    fn test_in_memory_pool() {
        let config = DbConfig::new(":memory:");
        let pool = config.build_pool();
        assert!(pool.is_ok());
    }

    #[test]
    fn invalid_pool_configuration_returns_typed_error() {
        let config = DbConfig {
            database_url: ":memory:".to_string(),
            max_connections: 0,
            connection_timeout: 30,
        };

        let error = config
            .build_pool()
            .expect_err("zero-sized pools are invalid");

        assert!(error.to_string().contains("failed to build database pool"));
        assert!(
            error
                .to_string()
                .contains("max_connections must be positive")
        );
        assert!(error.source_error().is_none());
    }
}
