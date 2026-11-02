//! API services coordinating repositories and external APIs.

use std::collections::HashMap;

use chrono::NaiveDateTime;

use crate::crypto::password::hash_password;
use crate::db::{
    AlbumRepository, ArtistInfoCacheRepository, ArtistRepository, BookmarkEntry,
    BookmarkRepository, DbPool, InternetRadioRepository, InternetRadioStation,
    MusicFolderRepository, MusicRepoError, MusicRepoErrorKind, NewUser, NowPlayingEntry,
    NowPlayingRepository, PlayQueue, PlayQueueRepository, PlaylistRepository, RatingRepository,
    RemoteCommand, RemoteControlRepository, RemoteSession, RemoteState, SETTING_LAST_SCAN_AT,
    ScrobbleRepository, SettingsRepository, SongRepository, StarredRepository, UserRepoError,
    UserRepoErrorKind, UserRepository, UserUpdate,
};
use crate::lastfm::{
    LastFmClient, LastFmError, models::extract_biography, models::extract_image_urls,
};
use crate::lyrics::{ExtractedLyrics, extract_lyrics};
use crate::models::User;
use crate::models::music::{
    Album, AlbumAnnotations, Artist, ArtistID3Response, ArtistInfo2Response, Song, SongAnnotations,
    saturating_i64_to_i32,
};
use crate::models::user::UserRoles;
use crate::paths::resolve_cover_art_dir;

const LASTFM_PLACEHOLDER_IMAGE_MARKER: &str = "2a96cbd8b46e442fc41c2b86b821562f";

#[derive(Debug, thiserror::Error)]
enum ArtistImageError {
    #[error("artist image IO failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("artist image HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("artist image request returned {0}")]
    HttpStatus(reqwest::StatusCode),
}

fn is_lastfm_placeholder_image(url: &str) -> bool {
    url.contains(LASTFM_PLACEHOLDER_IMAGE_MARKER)
}

/// Convert a Subsonic `time` parameter to a Last.fm timestamp.
///
/// Subsonic sends `time` in milliseconds since the Unix epoch, while
/// Last.fm expects seconds. Values already in seconds are left unchanged.
const fn normalize_lastfm_timestamp(timestamp: i64) -> i64 {
    const SECONDS_THRESHOLD: i64 = 10_000_000_000;

    if timestamp > SECONDS_THRESHOLD {
        timestamp / 1_000
    } else {
        timestamp
    }
}

/// Log a failed Last.fm request, clearing the stored session key when
/// Last.fm reports it as invalid (error 9) so the user can re-authenticate.
fn log_lastfm_failure(pool: &DbPool, user_id: i32, error: &LastFmError, operation: &str) {
    if matches!(error, LastFmError::Api { code: 9, .. }) {
        tracing::error!(
            user_id,
            operation,
            "Last.fm session key invalid; cleared, re-link via CLI"
        );
        if let Err(e) = UserRepository::new(pool.clone()).set_lastfm_session_key(user_id, None) {
            tracing::warn!(error = %e, "Failed to clear invalid Last.fm session key");
        }
    } else {
        tracing::warn!(error = %error, operation, "Last.fm request failed");
    }
}

async fn download_artist_image(
    client: &reqwest::Client,
    artist_id: i32,
    image_url: &str,
) -> Result<String, ArtistImageError> {
    use tokio::io::AsyncWriteExt;

    let cover_art_dir = resolve_cover_art_dir();
    tokio::fs::create_dir_all(&cover_art_dir).await?;

    let image_extension = std::path::Path::new(image_url).extension();
    let extension =
        if image_extension.is_some_and(|extension| extension.eq_ignore_ascii_case("png")) {
            "png"
        } else if image_extension.is_some_and(|extension| extension.eq_ignore_ascii_case("gif")) {
            "gif"
        } else {
            "jpg"
        };

    let cover_art_id = format!("artist-{artist_id}");
    let filepath = cover_art_dir.join(format!("{cover_art_id}.{extension}"));
    if tokio::fs::try_exists(&filepath).await? {
        return Ok(cover_art_id);
    }

    let response = client.get(image_url).send().await?;
    if !response.status().is_success() {
        return Err(ArtistImageError::HttpStatus(response.status()));
    }

    let bytes = response.bytes().await?;
    let mut file = tokio::fs::File::create(&filepath).await?;
    file.write_all(&bytes).await?;

    Ok(cover_art_id)
}

/// Build the shared HTTP client used for external requests (Last.fm metadata,
/// artist images). A single configured client enables connection reuse and
/// consistent timeouts.
fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:109.0) Gecko/20100101 Firefox/115.0")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("reqwest client configuration is static and valid")
}

/// Music library and playback operations.
#[derive(Clone, Debug)]
pub struct MusicLibrary {
    pool: DbPool,
    lastfm_client: LastFmClient,
    http_client: reqwest::Client,
}

/// A song with current-user annotations attached.
#[derive(Debug, Clone)]
pub(in crate::api) struct AnnotatedSong {
    pub(crate) song: Song,
    pub(crate) annotations: SongAnnotations,
}

/// An album with current-user annotations attached.
#[derive(Debug, Clone)]
pub(in crate::api) struct AnnotatedAlbum {
    pub(crate) album: Album,
    pub(crate) annotations: AlbumAnnotations,
    pub(crate) starred_at: Option<NaiveDateTime>,
}

/// A user's starred artists, albums, and songs with annotations.
#[derive(Debug, Clone)]
pub(in crate::api) struct StarredItems {
    /// Starred artists with their starred timestamp.
    pub(crate) artists: Vec<(Artist, NaiveDateTime)>,
    /// Album count per starred artist, keyed by artist ID.
    pub(crate) album_counts: HashMap<i32, i64>,
    /// Starred albums with annotations.
    pub(crate) albums: Vec<AnnotatedAlbum>,
    /// Starred songs with annotations.
    pub(crate) songs: Vec<AnnotatedSong>,
}

impl MusicLibrary {
    /// Create a new music library with a shared HTTP client.
    #[must_use]
    pub fn new(pool: DbPool, lastfm_client: LastFmClient) -> Self {
        Self::with_http_client(pool, lastfm_client, build_http_client())
    }

    /// Create a music library with an explicit shared HTTP client.
    #[must_use]
    pub const fn with_http_client(
        pool: DbPool,
        lastfm_client: LastFmClient,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            pool,
            lastfm_client,
            http_client,
        }
    }

    // ========================================================================
    // Music folders
    // ========================================================================

    pub(in crate::api) fn get_music_folders(
        &self,
    ) -> Result<Vec<crate::models::music::MusicFolder>, MusicRepoError> {
        MusicFolderRepository::new(self.pool.clone()).find_enabled()
    }

    pub(in crate::api) fn media_roots(
        &self,
    ) -> Result<Vec<crate::models::music::MusicFolder>, MusicRepoError> {
        MusicFolderRepository::new(self.pool.clone()).find_all()
    }

    pub(in crate::api) fn audio_source(
        &self,
        song_id: i32,
    ) -> Result<Option<crate::db::repo::music::SongAudioSource>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).audio_source(song_id)
    }

    pub(in crate::api) fn get_music_folder(
        &self,
        id: i32,
    ) -> Result<Option<crate::models::music::MusicFolder>, MusicRepoError> {
        Ok(MusicFolderRepository::new(self.pool.clone())
            .find_by_id(id)?
            .filter(|folder| folder.parent.is_none()))
    }

    // ========================================================================
    // Artists
    // ========================================================================

    pub(in crate::api) fn get_artists(
        &self,
        root: Option<i32>,
    ) -> Result<Vec<Artist>, MusicRepoError> {
        let artists = ArtistRepository::new(self.pool.clone());
        root.map_or_else(
            || artists.find_all(),
            |root| artists.find_by_music_folder(root),
        )
    }

    pub(in crate::api) fn get_artists_by_music_folder(
        &self,
        folder_id: i32,
    ) -> Result<Vec<Artist>, MusicRepoError> {
        ArtistRepository::new(self.pool.clone()).find_by_music_folder(folder_id)
    }

    pub(in crate::api) fn get_artists_last_modified(
        &self,
    ) -> Result<Option<NaiveDateTime>, MusicRepoError> {
        ArtistRepository::new(self.pool.clone()).get_last_modified()
    }

    /// Last library scan completion time in epoch millis (for lastModified).
    pub(in crate::api) fn get_last_scan_at_ms(&self) -> Result<Option<i64>, MusicRepoError> {
        let value = SettingsRepository::new(self.pool.clone()).get(SETTING_LAST_SCAN_AT)?;
        Ok(value.and_then(|value| value.parse::<i64>().ok()))
    }

    pub(in crate::api) fn get_artist_album_count(
        &self,
        artist_id: i32,
    ) -> Result<i64, MusicRepoError> {
        ArtistRepository::new(self.pool.clone()).count_albums(artist_id)
    }

    pub(in crate::api) fn get_artist(
        &self,
        artist_id: i32,
    ) -> Result<Option<Artist>, MusicRepoError> {
        ArtistRepository::new(self.pool.clone()).find_by_id(artist_id)
    }

    pub(in crate::api) fn get_artist_album_counts_batch(
        &self,
        artist_ids: &[i32],
    ) -> Result<HashMap<i32, i64>, MusicRepoError> {
        ArtistRepository::new(self.pool.clone()).count_albums_batch(artist_ids)
    }

    // ========================================================================
    // Albums
    // ========================================================================

    pub(in crate::api) fn get_album(&self, album_id: i32) -> Result<Option<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_by_id(album_id)
    }

    pub(in crate::api) fn get_albums_by_artist(
        &self,
        artist_id: i32,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_by_artist(artist_id)
    }

    pub(in crate::api) fn get_albums_alphabetical_by_name(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_alphabetical_by_name(
            offset,
            limit,
            music_folder_id,
        )
    }

    pub(in crate::api) fn get_albums_alphabetical_by_artist(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_alphabetical_by_artist(
            offset,
            limit,
            music_folder_id,
        )
    }

    pub(in crate::api) fn get_albums_newest(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_newest(offset, limit, music_folder_id)
    }

    pub(in crate::api) fn get_albums_frequent(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_frequent(offset, limit, music_folder_id)
    }

    pub(in crate::api) fn get_albums_recent(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_recent(offset, limit, music_folder_id)
    }

    pub(in crate::api) fn get_albums_random(
        &self,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_random(limit, music_folder_id)
    }

    pub(in crate::api) fn get_albums_by_year(
        &self,
        from_year: i32,
        to_year: i32,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_by_year_range(
            from_year,
            to_year,
            offset,
            limit,
            music_folder_id,
        )
    }

    pub(in crate::api) fn get_albums_by_genre(
        &self,
        genre: &str,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).find_by_genre(genre, offset, limit, music_folder_id)
    }

    pub(in crate::api) fn get_albums_starred(
        &self,
        user_id: i32,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let starred = StarredRepository::new(self.pool.clone()).get_starred_albums_paginated(
            user_id,
            offset,
            limit,
            music_folder_id,
        )?;
        Ok(starred.into_iter().map(|(album, _)| album).collect())
    }

    pub(in crate::api) fn get_albums_highest(
        &self,
        user_id: i32,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let album_ids = RatingRepository::new(self.pool.clone()).get_highest_rated_album_ids(
            user_id,
            offset,
            limit,
            music_folder_id,
        )?;

        if album_ids.is_empty() {
            return Ok(vec![]);
        }

        let albums = AlbumRepository::new(self.pool.clone()).find_by_ids(&album_ids)?;
        let mut album_map: HashMap<i32, Album> = albums.into_iter().map(|a| (a.id, a)).collect();

        Ok(album_ids
            .into_iter()
            .filter_map(|id| album_map.remove(&id))
            .collect())
    }

    // ========================================================================
    // Songs
    // ========================================================================

    pub(in crate::api) fn get_song(&self, song_id: i32) -> Result<Option<Song>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).find_by_id(song_id)
    }

    pub(in crate::api) fn find_song_by_artist_and_title(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Option<Song>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).find_by_artist_and_title(artist, title)
    }

    pub(in crate::api) fn get_songs_by_album(
        &self,
        album_id: i32,
    ) -> Result<Vec<Song>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).find_by_album(album_id)
    }

    pub(in crate::api) fn get_songs_by_artist(
        &self,
        artist_id: i32,
    ) -> Result<Vec<Song>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).find_by_artist(artist_id)
    }

    pub(in crate::api) fn get_random_songs(
        &self,
        size: i64,
        genre: Option<&str>,
        from_year: Option<i32>,
        to_year: Option<i32>,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Song>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).find_random(
            size,
            genre,
            from_year,
            to_year,
            music_folder_id,
        )
    }

    pub(in crate::api) fn get_songs_by_genre(
        &self,
        genre: &str,
        count: i64,
        offset: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Song>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).find_by_genre(genre, count, offset, music_folder_id)
    }

    pub(in crate::api) fn get_similar_songs_by_artist(
        &self,
        artist_id: i32,
        exclude_song_id: i32,
        limit: i64,
    ) -> Result<Vec<Song>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).find_random_by_artist(
            artist_id,
            exclude_song_id,
            limit,
        )
    }

    pub(in crate::api) fn get_top_songs_by_artist_name(
        &self,
        artist_name: &str,
        limit: i64,
    ) -> Result<Vec<Song>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).find_top_by_artist_name(artist_name, limit)
    }

    pub(in crate::api) fn get_song_lyrics(
        &self,
        song_id: i32,
    ) -> Result<Vec<ExtractedLyrics>, MusicRepoError> {
        let Some(song) = self.get_song(song_id)? else {
            return Ok(Vec::new());
        };
        Ok(extract_lyrics(std::path::Path::new(&song.path)))
    }

    // ========================================================================
    // Genres & search
    // ========================================================================

    pub(in crate::api) fn get_genres(&self) -> Result<Vec<(String, i64, i64)>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).get_genres()
    }

    pub(in crate::api) fn search_artists(
        &self,
        query: &str,
        music_folder_id: Option<i32>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Artist>, MusicRepoError> {
        ArtistRepository::new(self.pool.clone()).search(query, music_folder_id, offset, limit)
    }

    pub(in crate::api) fn search_albums(
        &self,
        query: &str,
        music_folder_id: Option<i32>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Album>, MusicRepoError> {
        AlbumRepository::new(self.pool.clone()).search(query, music_folder_id, offset, limit)
    }

    pub(in crate::api) fn search_songs(
        &self,
        query: &str,
        music_folder_id: Option<i32>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Song>, MusicRepoError> {
        SongRepository::new(self.pool.clone()).search(query, music_folder_id, offset, limit)
    }

    // ========================================================================
    // Starred
    // ========================================================================

    pub(in crate::api) fn star_artist(
        &self,
        user_id: i32,
        artist_id: i32,
    ) -> Result<(), MusicRepoError> {
        StarredRepository::new(self.pool.clone()).star_artist(user_id, artist_id)
    }

    pub(in crate::api) fn star_album(
        &self,
        user_id: i32,
        album_id: i32,
    ) -> Result<(), MusicRepoError> {
        StarredRepository::new(self.pool.clone()).star_album(user_id, album_id)
    }

    pub(in crate::api) fn star_song(
        &self,
        user_id: i32,
        song_id: i32,
    ) -> Result<(), MusicRepoError> {
        StarredRepository::new(self.pool.clone()).star_song(user_id, song_id)
    }

    pub(in crate::api) fn unstar_artist(
        &self,
        user_id: i32,
        artist_id: i32,
    ) -> Result<bool, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).unstar_artist(user_id, artist_id)
    }

    pub(in crate::api) fn unstar_album(
        &self,
        user_id: i32,
        album_id: i32,
    ) -> Result<bool, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).unstar_album(user_id, album_id)
    }

    pub(in crate::api) fn unstar_song(
        &self,
        user_id: i32,
        song_id: i32,
    ) -> Result<bool, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).unstar_song(user_id, song_id)
    }

    pub(in crate::api) fn get_starred_artists(
        &self,
        user_id: i32,
    ) -> Result<Vec<(Artist, NaiveDateTime)>, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).get_starred_artists(user_id)
    }

    pub(in crate::api) fn get_starred_albums(
        &self,
        user_id: i32,
    ) -> Result<Vec<(Album, NaiveDateTime)>, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).get_starred_albums(user_id)
    }

    pub(in crate::api) fn get_starred_songs(
        &self,
        user_id: i32,
    ) -> Result<Vec<(Song, NaiveDateTime)>, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).get_starred_songs(user_id)
    }

    pub(in crate::api) fn get_starred_at_for_artist(
        &self,
        user_id: i32,
        artist_id: i32,
    ) -> Result<Option<NaiveDateTime>, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).get_starred_at_for_artist(user_id, artist_id)
    }

    pub(in crate::api) fn get_starred_at_for_album(
        &self,
        user_id: i32,
        album_id: i32,
    ) -> Result<Option<NaiveDateTime>, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).get_starred_at_for_album(user_id, album_id)
    }

    pub(in crate::api) fn get_starred_at_for_song(
        &self,
        user_id: i32,
        song_id: i32,
    ) -> Result<Option<NaiveDateTime>, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).get_starred_at_for_song(user_id, song_id)
    }

    pub(in crate::api) fn get_starred_at_for_songs_batch(
        &self,
        user_id: i32,
        song_ids: &[i32],
    ) -> Result<HashMap<i32, NaiveDateTime>, MusicRepoError> {
        StarredRepository::new(self.pool.clone()).get_starred_at_for_songs_batch(user_id, song_ids)
    }

    pub(in crate::api) fn get_starred_at_for_albums_batch(
        &self,
        user_id: i32,
        album_ids: &[i32],
    ) -> Result<HashMap<i32, NaiveDateTime>, MusicRepoError> {
        StarredRepository::new(self.pool.clone())
            .get_starred_at_for_albums_batch(user_id, album_ids)
    }

    pub(in crate::api) fn get_starred_at_for_artists_batch(
        &self,
        user_id: i32,
        artist_ids: &[i32],
    ) -> Result<HashMap<i32, NaiveDateTime>, MusicRepoError> {
        StarredRepository::new(self.pool.clone())
            .get_starred_at_for_artists_batch(user_id, artist_ids)
    }

    // ========================================================================
    // Song & album annotations (ratings, last played, bookmarks)
    // ========================================================================

    /// Attach current-user annotations to a set of songs, preserving input
    /// order and duplicates.
    pub(in crate::api) fn annotate_songs_for_user(
        &self,
        user_id: i32,
        songs: Vec<Song>,
    ) -> Result<Vec<AnnotatedSong>, MusicRepoError> {
        let song_ids: Vec<i32> = songs.iter().map(|song| song.id).collect();
        let starred = self.get_starred_at_for_songs_batch(user_id, &song_ids)?;
        self.annotate_songs_with_starred(user_id, songs, &starred)
    }

    /// Attach current-user annotations to songs whose starred timestamps are
    /// already known (e.g. from a starred-items query).
    pub(in crate::api) fn annotate_songs_with_starred(
        &self,
        user_id: i32,
        songs: Vec<Song>,
        starred: &HashMap<i32, NaiveDateTime>,
    ) -> Result<Vec<AnnotatedSong>, MusicRepoError> {
        let song_ids: Vec<i32> = songs.iter().map(|song| song.id).collect();
        if song_ids.is_empty() {
            return Ok(Vec::new());
        }

        let annotations = self.get_song_annotations_batch(user_id, &song_ids)?;

        Ok(songs
            .into_iter()
            .map(|song| {
                let mut entry = annotations.get(&song.id).copied().unwrap_or_default();
                entry.starred_at = starred.get(&song.id).copied();
                AnnotatedSong {
                    song,
                    annotations: entry,
                }
            })
            .collect())
    }

    /// Attach current-user annotations to a set of albums, preserving input
    /// order and duplicates.
    pub(in crate::api) fn annotate_albums_for_user(
        &self,
        user_id: i32,
        albums: Vec<Album>,
    ) -> Result<Vec<AnnotatedAlbum>, MusicRepoError> {
        let album_ids: Vec<i32> = albums.iter().map(|album| album.id).collect();
        let starred = self.get_starred_at_for_albums_batch(user_id, &album_ids)?;
        self.annotate_albums_with_starred(user_id, albums, &starred)
    }

    /// Attach current-user annotations to albums whose starred timestamps are
    /// already known (e.g. from a starred-items query).
    pub(in crate::api) fn annotate_albums_with_starred(
        &self,
        user_id: i32,
        albums: Vec<Album>,
        starred: &HashMap<i32, NaiveDateTime>,
    ) -> Result<Vec<AnnotatedAlbum>, MusicRepoError> {
        let album_ids: Vec<i32> = albums.iter().map(|album| album.id).collect();
        if album_ids.is_empty() {
            return Ok(Vec::new());
        }

        let annotations = self.get_album_annotations_batch(user_id, &album_ids)?;

        Ok(albums
            .into_iter()
            .map(|album| {
                let entry = annotations.get(&album.id).copied().unwrap_or_default();
                let starred_at = starred.get(&album.id).copied();
                AnnotatedAlbum {
                    album,
                    annotations: entry,
                    starred_at,
                }
            })
            .collect())
    }

    /// Fetch per-user annotation data for a set of songs, keyed by song ID.
    pub(in crate::api) fn get_song_annotations_batch(
        &self,
        user_id: i32,
        song_ids: &[i32],
    ) -> Result<HashMap<i32, SongAnnotations>, MusicRepoError> {
        let ratings =
            RatingRepository::new(self.pool.clone()).get_song_ratings_batch(user_id, song_ids)?;
        let averages =
            RatingRepository::new(self.pool.clone()).get_average_song_ratings_batch(song_ids)?;
        let last_played = ScrobbleRepository::new(self.pool.clone())
            .get_last_played_for_songs_batch(user_id, song_ids)?;
        let bookmark_positions = BookmarkRepository::new(self.pool.clone())
            .get_positions_for_songs(user_id, song_ids)?;

        let mut annotations = HashMap::with_capacity(song_ids.len());
        for song_id in song_ids {
            let entry = SongAnnotations {
                starred_at: None,
                user_rating: ratings.get(song_id).copied(),
                average_rating: averages.get(song_id).copied(),
                played_at: last_played.get(song_id).copied(),
                bookmark_position: bookmark_positions.get(song_id).copied(),
            };
            if entry.user_rating.is_some()
                || entry.average_rating.is_some()
                || entry.played_at.is_some()
                || entry.bookmark_position.is_some()
            {
                annotations.insert(*song_id, entry);
            }
        }
        Ok(annotations)
    }

    /// Fetch per-user annotation data for a set of albums, keyed by album ID.
    pub(in crate::api) fn get_album_annotations_batch(
        &self,
        user_id: i32,
        album_ids: &[i32],
    ) -> Result<HashMap<i32, AlbumAnnotations>, MusicRepoError> {
        let ratings =
            RatingRepository::new(self.pool.clone()).get_album_ratings_batch(user_id, album_ids)?;
        let averages =
            RatingRepository::new(self.pool.clone()).get_average_album_ratings_batch(album_ids)?;
        let last_played = ScrobbleRepository::new(self.pool.clone())
            .get_last_played_for_albums_batch(user_id, album_ids)?;

        let mut annotations = HashMap::with_capacity(album_ids.len());
        for album_id in album_ids {
            let entry = AlbumAnnotations {
                user_rating: ratings.get(album_id).copied(),
                average_rating: averages.get(album_id).copied(),
                played_at: last_played.get(album_id).copied(),
            };
            if entry.user_rating.is_some()
                || entry.average_rating.is_some()
                || entry.played_at.is_some()
            {
                annotations.insert(*album_id, entry);
            }
        }
        Ok(annotations)
    }

    // ========================================================================
    // Starred items
    // ========================================================================

    /// Fetch a user's starred artists, albums, and songs with annotations
    /// attached, plus each artist's album count. This is the deep aggregate
    /// behind `getStarred`/`getStarred2`: one call hides the starred-item
    /// queries and the annotation batches.
    pub(in crate::api) fn starred_items_for_user(
        &self,
        user_id: i32,
    ) -> Result<StarredItems, MusicRepoError> {
        let starred_artists = self.get_starred_artists(user_id)?;
        let artist_ids: Vec<i32> = starred_artists.iter().map(|(a, _)| a.id).collect();
        let album_counts = self.get_artist_album_counts_batch(&artist_ids)?;

        let starred_albums = self.get_starred_albums(user_id)?;
        let album_starred: HashMap<i32, NaiveDateTime> = starred_albums
            .iter()
            .map(|(album, starred_at)| (album.id, *starred_at))
            .collect();
        let annotated_albums = self.annotate_albums_with_starred(
            user_id,
            starred_albums.into_iter().map(|(album, _)| album).collect(),
            &album_starred,
        )?;

        let starred_songs = self.get_starred_songs(user_id)?;
        let song_starred: HashMap<i32, NaiveDateTime> = starred_songs
            .iter()
            .map(|(song, starred_at)| (song.id, *starred_at))
            .collect();
        let annotated_songs = self.annotate_songs_with_starred(
            user_id,
            starred_songs.into_iter().map(|(song, _)| song).collect(),
            &song_starred,
        )?;

        Ok(StarredItems {
            artists: starred_artists,
            album_counts,
            albums: annotated_albums,
            songs: annotated_songs,
        })
    }

    // ========================================================================
    // Annotated entity details
    // ========================================================================

    /// Fetch a song with the current user's annotations attached.
    ///
    /// Returns `Ok(None)` when the song does not exist. This is the deep
    /// aggregate behind `getSong` and friends: one call hides the starred-at
    /// lookup and the annotation batch query.
    pub(in crate::api) fn annotated_song_for_user(
        &self,
        user_id: i32,
        song_id: i32,
    ) -> Result<Option<AnnotatedSong>, MusicRepoError> {
        let Some(song) = self.get_song(song_id)? else {
            return Ok(None);
        };
        let starred = self.get_starred_at_for_song(user_id, song_id)?;
        let annotations = self.get_song_annotations_batch(user_id, &[song_id])?;
        let mut entry = annotations.get(&song_id).copied().unwrap_or_default();
        entry.starred_at = starred;
        Ok(Some(AnnotatedSong {
            song,
            annotations: entry,
        }))
    }

    /// Fetch an album with the current user's annotations attached.
    ///
    /// Returns `Ok(None)` when the album does not exist.
    pub(in crate::api) fn annotated_album_for_user(
        &self,
        user_id: i32,
        album_id: i32,
    ) -> Result<Option<AnnotatedAlbum>, MusicRepoError> {
        let Some(album) = self.get_album(album_id)? else {
            return Ok(None);
        };
        let starred_at = self.get_starred_at_for_album(user_id, album_id)?;
        let annotations = self.get_album_annotations_batch(user_id, &[album_id])?;
        let entry = annotations.get(&album_id).copied().unwrap_or_default();
        Ok(Some(AnnotatedAlbum {
            album,
            annotations: entry,
            starred_at,
        }))
    }

    // ========================================================================
    // Bookmarks
    // ========================================================================

    pub(in crate::api) fn get_bookmarks(
        &self,
        user_id: i32,
    ) -> Result<Vec<BookmarkEntry>, MusicRepoError> {
        BookmarkRepository::new(self.pool.clone()).get_for_user(user_id)
    }

    pub(in crate::api) fn create_bookmark(
        &self,
        user_id: i32,
        song_id: i32,
        position: i64,
        comment: Option<&str>,
    ) -> Result<(), MusicRepoError> {
        BookmarkRepository::new(self.pool.clone()).upsert(user_id, song_id, position, comment)
    }

    pub(in crate::api) fn delete_bookmark(
        &self,
        user_id: i32,
        song_id: i32,
    ) -> Result<bool, MusicRepoError> {
        BookmarkRepository::new(self.pool.clone()).delete(user_id, song_id)
    }

    // ========================================================================
    // Internet radio stations
    // ========================================================================

    pub(in crate::api) fn get_internet_radio_stations(
        &self,
    ) -> Result<Vec<InternetRadioStation>, MusicRepoError> {
        InternetRadioRepository::new(self.pool.clone()).find_all()
    }

    pub(in crate::api) fn create_internet_radio_station(
        &self,
        name: &str,
        stream_url: &str,
        home_page_url: Option<&str>,
    ) -> Result<(), MusicRepoError> {
        InternetRadioRepository::new(self.pool.clone()).create(name, stream_url, home_page_url)
    }

    pub(in crate::api) fn update_internet_radio_station(
        &self,
        id: i32,
        name: &str,
        stream_url: &str,
        home_page_url: Option<&str>,
    ) -> Result<bool, MusicRepoError> {
        InternetRadioRepository::new(self.pool.clone()).update(id, name, stream_url, home_page_url)
    }

    pub(in crate::api) fn delete_internet_radio_station(
        &self,
        id: i32,
    ) -> Result<bool, MusicRepoError> {
        InternetRadioRepository::new(self.pool.clone()).delete(id)
    }

    // ========================================================================
    // Scrobble & now playing
    // ========================================================================

    pub(in crate::api) fn scrobble(
        &self,
        user_id: i32,
        song_id: i32,
        time: Option<i64>,
        submission: bool,
    ) -> Result<(), MusicRepoError> {
        ScrobbleRepository::new(self.pool.clone()).scrobble(user_id, song_id, time, submission)?;

        if submission && let Some(song) = self.get_song(song_id)? {
            let timestamp = time.map_or_else(
                || chrono::Utc::now().timestamp(),
                normalize_lastfm_timestamp,
            );
            self.submit_lastfm_scrobble(user_id, &song, timestamp);
        }

        Ok(())
    }

    pub(in crate::api) fn set_now_playing(
        &self,
        user_id: i32,
        song_id: i32,
        player_id: Option<&str>,
    ) -> Result<(), MusicRepoError> {
        NowPlayingRepository::new(self.pool.clone())
            .set_now_playing(user_id, song_id, player_id)?;

        if let Some(song) = self.get_song(song_id)? {
            self.update_lastfm_now_playing(user_id, &song);
        }

        Ok(())
    }

    pub(in crate::api) fn get_now_playing(&self) -> Result<Vec<NowPlayingEntry>, MusicRepoError> {
        NowPlayingRepository::new(self.pool.clone()).get_all_now_playing()
    }

    /// Load the user's Last.fm session key, logging why a request is skipped.
    fn lastfm_session_key(&self, user_id: i32) -> Option<String> {
        match UserRepository::new(self.pool.clone()).get_lastfm_session_key(user_id) {
            Ok(Some(key)) => Some(key),
            Ok(None) => {
                tracing::info!(
                    user_id,
                    "no Last.fm session key for user; skipping. \
                     Link with `suboxide lastfm link`"
                );
                None
            }
            Err(e) => {
                tracing::warn!(user_id, error = %e, "failed to load Last.fm session key");
                None
            }
        }
    }

    fn submit_lastfm_scrobble(&self, user_id: i32, song: &Song, timestamp: i64) {
        if !self.lastfm_client.is_configured() {
            return;
        }

        let Some(session_key) = self.lastfm_session_key(user_id) else {
            return;
        };

        let client = self.lastfm_client.clone();
        let artist = song.artist_name.clone().unwrap_or_default();
        let track = song.title.clone();
        let album = song.album_name.clone();
        let pool = self.pool.clone();

        tokio::spawn(async move {
            if let Err(e) = client
                .scrobble(&session_key, &artist, &track, album.as_deref(), timestamp)
                .await
            {
                log_lastfm_failure(&pool, user_id, &e, "scrobble");
            } else {
                tracing::debug!(artist = %artist, track = %track, "Submitted scrobble to Last.fm");
            }
        });
    }

    fn update_lastfm_now_playing(&self, user_id: i32, song: &Song) {
        if !self.lastfm_client.is_configured() {
            return;
        }

        let Some(session_key) = self.lastfm_session_key(user_id) else {
            return;
        };

        let client = self.lastfm_client.clone();
        let artist = song.artist_name.clone().unwrap_or_default();
        let track = song.title.clone();
        let album = song.album_name.clone();
        let duration = Some(song.duration);
        let pool = self.pool.clone();

        tokio::spawn(async move {
            if let Err(e) = client
                .update_now_playing(&session_key, &artist, &track, album.as_deref(), duration)
                .await
            {
                log_lastfm_failure(&pool, user_id, &e, "update now playing");
            } else {
                tracing::debug!(artist = %artist, track = %track, "Updated Last.fm now playing");
            }
        });
    }

    // ========================================================================
    // Ratings
    // ========================================================================

    pub(in crate::api) fn set_song_rating(
        &self,
        user_id: i32,
        song_id: i32,
        rating: i32,
    ) -> Result<(), MusicRepoError> {
        RatingRepository::new(self.pool.clone()).set_song_rating(user_id, song_id, rating)
    }

    pub(in crate::api) fn set_album_rating(
        &self,
        user_id: i32,
        album_id: i32,
        rating: i32,
    ) -> Result<(), MusicRepoError> {
        RatingRepository::new(self.pool.clone()).set_album_rating(user_id, album_id, rating)
    }

    pub(in crate::api) fn set_artist_rating(
        &self,
        user_id: i32,
        artist_id: i32,
        rating: i32,
    ) -> Result<(), MusicRepoError> {
        RatingRepository::new(self.pool.clone()).set_artist_rating(user_id, artist_id, rating)
    }

    // ========================================================================
    // Playlists
    // ========================================================================

    pub(in crate::api) fn get_playlists(
        &self,
        user_id: i32,
        username: &str,
    ) -> Result<Vec<crate::db::Playlist>, MusicRepoError> {
        PlaylistRepository::new(self.pool.clone()).get_playlists(user_id, username)
    }

    pub(in crate::api) fn get_playlist(
        &self,
        playlist_id: i32,
    ) -> Result<Option<crate::db::Playlist>, MusicRepoError> {
        PlaylistRepository::new(self.pool.clone()).get_playlist(playlist_id)
    }

    pub(in crate::api) fn get_playlist_songs(
        &self,
        playlist_id: i32,
    ) -> Result<Vec<Song>, MusicRepoError> {
        PlaylistRepository::new(self.pool.clone()).get_playlist_songs(playlist_id)
    }

    pub(in crate::api) fn get_playlist_cover_arts_batch(
        &self,
        playlist_ids: &[i32],
    ) -> Result<HashMap<i32, String>, MusicRepoError> {
        PlaylistRepository::new(self.pool.clone()).get_playlist_cover_arts_batch(playlist_ids)
    }

    pub(in crate::api) fn create_playlist(
        &self,
        user_id: i32,
        name: &str,
        comment: Option<&str>,
        song_ids: &[i32],
    ) -> Result<crate::db::Playlist, MusicRepoError> {
        PlaylistRepository::new(self.pool.clone()).create_playlist(user_id, name, comment, song_ids)
    }

    pub(in crate::api) fn update_playlist(
        &self,
        playlist_id: i32,
        name: Option<&str>,
        comment: Option<&str>,
        public: Option<bool>,
        song_ids_to_add: &[i32],
        song_indices_to_remove: &[i32],
    ) -> Result<(), MusicRepoError> {
        PlaylistRepository::new(self.pool.clone()).update_playlist(
            playlist_id,
            name,
            comment,
            public,
            song_ids_to_add,
            song_indices_to_remove,
        )
    }

    pub(in crate::api) fn delete_playlist(&self, playlist_id: i32) -> Result<bool, MusicRepoError> {
        PlaylistRepository::new(self.pool.clone()).delete_playlist(playlist_id)
    }

    pub(in crate::api) fn is_playlist_owner(
        &self,
        user_id: i32,
        playlist_id: i32,
    ) -> Result<bool, MusicRepoError> {
        PlaylistRepository::new(self.pool.clone()).is_owner(user_id, playlist_id)
    }

    // ========================================================================
    // Play queue
    // ========================================================================

    pub(in crate::api) fn get_play_queue(
        &self,
        user_id: i32,
        username: &str,
    ) -> Result<Option<PlayQueue>, MusicRepoError> {
        PlayQueueRepository::new(self.pool.clone()).get_play_queue(user_id, username)
    }

    pub(in crate::api) fn save_play_queue(
        &self,
        user_id: i32,
        song_ids: &[i32],
        current_song_id: Option<i32>,
        position: Option<i64>,
        changed_by: Option<&str>,
    ) -> Result<(), MusicRepoError> {
        PlayQueueRepository::new(self.pool.clone()).save_play_queue(
            user_id,
            song_ids,
            current_song_id,
            position,
            changed_by,
        )
    }

    // ========================================================================
    // Artist info with Last.fm cache
    // ========================================================================

    pub(in crate::api) fn get_artist_info_with_cache(
        &self,
        artist_id: i32,
    ) -> Result<ArtistInfo2Response, MusicRepoError> {
        let Some(artist) = self.get_artist(artist_id)? else {
            return Ok(ArtistInfo2Response::empty());
        };

        let mut response = ArtistInfo2Response::from_artist(&artist);

        tracing::debug!(artist_id = artist_id, artist = %artist.name, "Fetching artist info");

        match ArtistInfoCacheRepository::new(self.pool.clone()).get_valid_cache(artist_id) {
            Ok(Some(cache)) => {
                tracing::debug!(artist_id = artist_id, "Using cached Last.fm info");
                response.biography = cache.biography;
                response.last_fm_url = cache.last_fm_url;
                response.small_image_url = cache.small_image_url;
                response.medium_image_url = cache.medium_image_url;
                response.large_image_url = cache.large_image_url;

                for similar_name in &cache.similar_artists {
                    if let Some(similar_artist) =
                        ArtistRepository::new(self.pool.clone()).find_by_name(similar_name)?
                    {
                        let album_count = self.get_artist_album_count(similar_artist.id)?;
                        response
                            .similar_artists
                            .push(ArtistID3Response::from_artist(
                                &similar_artist,
                                Some(saturating_i64_to_i32(album_count)),
                            ));
                    }
                }
            }
            Ok(None) => {
                let service = self.clone();
                let artist_name = artist.name;
                tokio::spawn(async move {
                    service
                        .fetch_and_cache_artist_info(artist_id, artist_name)
                        .await;
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, artist_id = artist_id, "Failed to read artist cache");
            }
        }

        Ok(response)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Coordinates async Last.fm API call, image scraping, caching, and cover art download"
    )]
    async fn fetch_and_cache_artist_info(&self, artist_id: i32, artist_name: String) {
        use crate::lastfm::models::LastFmArtistCache;

        let client = self.lastfm_client.clone();
        let pool = self.pool.clone();

        match client.get_artist_info(&artist_name).await {
            Ok(Some(lastfm_artist)) => {
                let (mut small, mut medium, mut large) = extract_image_urls(&lastfm_artist.image);

                if let Some(ref page_url) = lastfm_artist.url {
                    tracing::debug!(
                        artist = %artist_name,
                        url = %page_url,
                        "Attempting to scrape artist image from page"
                    );
                    match client.fetch_artist_image_from_page(page_url).await {
                        Ok(Some(scraped_url)) => {
                            tracing::debug!(
                                artist = %artist_name,
                                url = %scraped_url,
                                "Successfully scraped artist image"
                            );
                            large = Some(scraped_url);
                            if small.is_none() {
                                small = large.clone();
                            }
                            if medium.is_none() {
                                medium = large.clone();
                            }
                        }
                        Ok(None) => {
                            tracing::debug!(
                                artist = %artist_name,
                                "No image found on scraped page"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                artist = %artist_name,
                                "Failed to scrape artist page"
                            );
                        }
                    }
                }

                tracing::debug!(
                    artist = %artist_name,
                    small = ?small,
                    medium = ?medium,
                    large = ?large,
                    "Final Last.fm image URLs"
                );
                let bio = extract_biography(&lastfm_artist.bio);

                let similar_names: Vec<String> = lastfm_artist
                    .similar
                    .artist
                    .iter()
                    .map(|a| a.name.clone())
                    .collect();

                if large.as_deref().is_some_and(is_lastfm_placeholder_image) {
                    tracing::warn!(
                        artist = %artist_name,
                        url = ?large,
                        "Discarding Last.fm placeholder image"
                    );
                    small = small.filter(|url| !is_lastfm_placeholder_image(url));
                    medium = medium.filter(|url| !is_lastfm_placeholder_image(url));
                    large = None;
                }

                let cache = LastFmArtistCache {
                    artist_id,
                    biography: bio,
                    last_fm_url: lastfm_artist.url,
                    small_image_url: small,
                    medium_image_url: medium,
                    large_image_url: large.clone(),
                    similar_artists: similar_names,
                    updated_at: chrono::Local::now().naive_local(),
                };

                if let Err(e) = ArtistInfoCacheRepository::new(pool.clone()).save_cache(&cache) {
                    tracing::warn!(error = %e, "Failed to save artist cache");
                } else {
                    tracing::debug!(artist = %artist_name, "Cached Last.fm artist info");
                }

                if let Some(image_url) = large {
                    match download_artist_image(&self.http_client, artist_id, &image_url).await {
                        Ok(cover_art_id) => {
                            tracing::debug!(artist = %artist_name, "Downloaded artist image");
                            if let Err(e) = ArtistRepository::new(pool.clone())
                                .update_cover_art(artist_id, Some(&cover_art_id))
                            {
                                tracing::warn!(error = %e, "Failed to update artist cover art");
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                error = %error,
                                artist = %artist_name,
                                url = %image_url,
                                "Failed to store artist image"
                            );
                        }
                    }
                }
            }
            Ok(None) => {
                tracing::debug!(artist = %artist_name, "No Last.fm info found");
            }
            Err(e) => {
                tracing::warn!(error = %e, artist = %artist_name, "Failed to fetch Last.fm artist info");
            }
        }
    }

    pub(in crate::api) fn get_artist_info_non_id3_with_cache(
        &self,
        artist_id: i32,
    ) -> Result<crate::models::music::ArtistInfoResponse, MusicRepoError> {
        use crate::models::music::{ArtistInfoResponse, ArtistResponse};

        let info2 = self.get_artist_info_with_cache(artist_id)?;

        let similar_artists = info2
            .similar_artists
            .into_iter()
            .map(|a| ArtistResponse {
                id: a.id,
                name: a.name,
                artist_image_url: a.artist_image_url,
                starred: a.starred,
                user_rating: None,
                average_rating: None,
            })
            .collect();

        Ok(ArtistInfoResponse {
            biography: info2.biography,
            musicbrainz_id: info2.musicbrainz_id,
            last_fm_url: info2.last_fm_url,
            small_image_url: info2.small_image_url,
            medium_image_url: info2.medium_image_url,
            large_image_url: info2.large_image_url,
            similar_artists,
        })
    }
}

/// User accounts.
#[derive(Clone, Debug)]
pub struct Users {
    pool: DbPool,
}

impl Users {
    /// Create a user account handle.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub(in crate::api) fn find_user(&self, username: &str) -> Result<Option<User>, UserRepoError> {
        UserRepository::new(self.pool.clone()).find_by_username(username)
    }

    pub(in crate::api) fn find_user_by_api_key(
        &self,
        api_key: &str,
    ) -> Result<Option<User>, UserRepoError> {
        UserRepository::new(self.pool.clone()).find_by_api_key(api_key)
    }

    /// List all users.
    pub fn get_all_users(&self) -> Result<Vec<User>, UserRepoError> {
        UserRepository::new(self.pool.clone()).find_all()
    }

    /// Delete a user by name.
    pub fn delete_user(&self, username: &str) -> Result<bool, UserRepoError> {
        let user = UserRepository::new(self.pool.clone())
            .find_by_username(username)?
            .ok_or_else(|| {
                UserRepoError::new(
                    UserRepoErrorKind::NotFound,
                    format!("user '{username}' not found"),
                )
            })?;
        UserRepository::new(self.pool.clone()).delete(user.id)
    }

    pub(in crate::api) fn change_password(
        &self,
        username: &str,
        new_password: &str,
    ) -> Result<(), UserRepoError> {
        let user = UserRepository::new(self.pool.clone())
            .find_by_username(username)?
            .ok_or_else(|| {
                UserRepoError::new(
                    UserRepoErrorKind::NotFound,
                    format!("user '{username}' not found"),
                )
            })?;

        let password_hash = hash_password(new_password)
            .map_err(|error| UserRepoError::new(UserRepoErrorKind::Database, error.to_string()))?;

        if !UserRepository::new(self.pool.clone()).update_password(user.id, &password_hash)? {
            return Err(UserRepoError::new(
                UserRepoErrorKind::NotFound,
                format!("user '{username}' not found"),
            ));
        }
        if !UserRepository::new(self.pool.clone())
            .update_subsonic_password(user.id, new_password)?
        {
            return Err(UserRepoError::new(
                UserRepoErrorKind::NotFound,
                format!("user '{username}' not found"),
            ));
        }

        Ok(())
    }

    pub(in crate::api) fn create_user(
        &self,
        username: &str,
        password: &str,
        email: &str,
        roles: &UserRoles,
    ) -> Result<User, UserRepoError> {
        let password_hash = hash_password(password)
            .map_err(|error| UserRepoError::new(UserRepoErrorKind::Database, error.to_string()))?;

        let new_user = NewUser::builder(username, &password_hash)
            .subsonic_password(password)
            .email(email)
            .admin_role(roles.admin_role)
            .settings_role(roles.settings_role)
            .stream_role(roles.stream_role)
            .jukebox_role(roles.jukebox_role)
            .download_role(roles.download_role)
            .upload_role(roles.upload_role)
            .playlist_role(roles.playlist_role)
            .cover_art_role(roles.cover_art_role)
            .comment_role(roles.comment_role)
            .podcast_role(roles.podcast_role)
            .share_role(roles.share_role)
            .video_conversion_role(roles.video_conversion_role)
            .max_bit_rate(0)
            .build();

        UserRepository::new(self.pool.clone()).create(&new_user)
    }

    pub(in crate::api) fn update_user(&self, update: &UserUpdate) -> Result<bool, UserRepoError> {
        UserRepository::new(self.pool.clone()).update_user(update)
    }
}

/// Remote control sessions.
#[derive(Clone, Debug)]
pub struct RemoteSessions {
    pool: DbPool,
}

impl RemoteSessions {
    /// Create a remote session handle.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    pub(in crate::api) fn create_remote_session(
        &self,
        user_id: i32,
        host_device_id: &str,
        host_device_name: Option<&str>,
        ttl_seconds: i64,
    ) -> Result<RemoteSession, MusicRepoError> {
        RemoteControlRepository::new(self.pool.clone()).create_session(
            user_id,
            host_device_id,
            host_device_name,
            ttl_seconds,
        )
    }

    pub(in crate::api) fn join_remote_session(
        &self,
        user_id: i32,
        pairing_code: &str,
        controller_device_id: &str,
        controller_device_name: Option<&str>,
    ) -> Result<Option<RemoteSession>, MusicRepoError> {
        RemoteControlRepository::new(self.pool.clone()).join_session(
            pairing_code,
            user_id,
            controller_device_id,
            controller_device_name,
        )
    }

    pub(in crate::api) fn close_remote_session(
        &self,
        user_id: i32,
        session_id: &str,
    ) -> Result<bool, MusicRepoError> {
        RemoteControlRepository::new(self.pool.clone()).close_session(session_id, user_id)
    }

    pub(in crate::api) fn get_remote_session(
        &self,
        user_id: i32,
        session_id: &str,
    ) -> Result<Option<RemoteSession>, MusicRepoError> {
        RemoteControlRepository::new(self.pool.clone()).get_session_for_user(session_id, user_id)
    }

    /// Verify the session exists and belongs to the user, returning the
    /// canonical not-found error otherwise.
    fn require_session(&self, user_id: i32, session_id: &str) -> Result<(), MusicRepoError> {
        RemoteControlRepository::new(self.pool.clone())
            .get_session_for_user(session_id, user_id)?
            .ok_or_else(|| {
                MusicRepoError::new(MusicRepoErrorKind::NotFound, "remote session not found")
            })
            .map(|_| ())
    }

    pub(in crate::api) fn send_remote_command(
        &self,
        user_id: i32,
        session_id: &str,
        source_device_id: &str,
        command: &str,
        payload: Option<&str>,
    ) -> Result<i64, MusicRepoError> {
        self.require_session(user_id, session_id)?;

        RemoteControlRepository::new(self.pool.clone()).enqueue_command(
            session_id,
            source_device_id,
            command,
            payload,
        )
    }

    pub(in crate::api) fn get_remote_commands(
        &self,
        user_id: i32,
        session_id: &str,
        since_id: i64,
        limit: i64,
        exclude_device_id: &str,
    ) -> Result<Vec<RemoteCommand>, MusicRepoError> {
        self.require_session(user_id, session_id)?;

        RemoteControlRepository::new(self.pool.clone()).get_commands(
            session_id,
            since_id,
            limit,
            exclude_device_id,
        )
    }

    pub(in crate::api) fn update_remote_state(
        &self,
        user_id: i32,
        session_id: &str,
        updated_by_device_id: &str,
        state_json: &str,
    ) -> Result<(), MusicRepoError> {
        self.require_session(user_id, session_id)?;

        RemoteControlRepository::new(self.pool.clone()).update_state(
            session_id,
            updated_by_device_id,
            state_json,
        )
    }

    pub(in crate::api) fn get_remote_state(
        &self,
        user_id: i32,
        session_id: &str,
    ) -> Result<Option<RemoteState>, MusicRepoError> {
        self.require_session(user_id, session_id)?;

        RemoteControlRepository::new(self.pool.clone()).get_state(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{AnnotatedAlbum, AnnotatedSong, normalize_lastfm_timestamp};
    use crate::models::music::{Album, AlbumAnnotations, Song, SongAnnotations};
    use chrono::NaiveDate;

    fn song(id: i32) -> Song {
        let now = NaiveDate::from_ymd_opt(2024, 1, 1)
            .expect("valid date")
            .and_hms_opt(0, 0, 0)
            .expect("valid time");
        Song {
            id,
            title: format!("Song {id}"),
            sort_name: None,
            album_id: Some(1),
            artist_id: Some(1),
            artist_name: Some("Artist".into()),
            album_name: Some("Album".into()),
            music_folder_id: 1,
            path: format!("/music/song{id}.flac"),
            cue_track_id: None,
            parent_path: "/music".into(),
            file_size: 100,
            content_type: "audio/flac".into(),
            suffix: "flac".into(),
            duration: 60,
            bit_rate: None,
            bit_depth: None,
            sampling_rate: None,
            channel_count: None,
            track_number: Some(1),
            disc_number: Some(1),
            year: None,
            genre: None,
            cover_art: None,
            musicbrainz_id: None,
            play_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    fn album(id: i32) -> Album {
        let now = NaiveDate::from_ymd_opt(2024, 1, 1)
            .expect("valid date")
            .and_hms_opt(0, 0, 0)
            .expect("valid time");
        Album {
            id,
            name: format!("Album {id}"),
            sort_name: None,
            artist_id: Some(1),
            artist_name: Some("Artist".into()),
            year: None,
            genre: None,
            cover_art: None,
            musicbrainz_id: None,
            duration: 120,
            song_count: 2,
            play_count: 0,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn annotated_song_preserves_entity_and_annotations() {
        let item = AnnotatedSong {
            song: song(7),
            annotations: SongAnnotations {
                starred_at: None,
                user_rating: Some(4),
                average_rating: Some(3.5),
                played_at: None,
                bookmark_position: Some(1200),
            },
        };
        assert_eq!(item.song.id, 7);
        assert_eq!(item.annotations.user_rating, Some(4));
        assert_eq!(item.annotations.bookmark_position, Some(1200));
    }

    #[test]
    fn annotated_album_preserves_entity_annotations_and_starred_time() {
        let item = AnnotatedAlbum {
            album: album(3),
            annotations: AlbumAnnotations {
                user_rating: Some(5),
                average_rating: Some(4.0),
                played_at: None,
            },
            starred_at: None,
        };
        assert_eq!(item.album.id, 3);
        assert_eq!(item.annotations.user_rating, Some(5));
        assert_eq!(item.annotations.average_rating, Some(4.0));
        assert!(item.starred_at.is_none());
    }

    #[test]
    fn milliseconds_timestamp_is_converted_to_seconds() {
        // 2024-01-01 00:00:00 UTC in milliseconds.
        let ms = 1_704_067_200_000_i64;
        assert_eq!(normalize_lastfm_timestamp(ms), 1_704_067_200);
    }

    #[test]
    fn seconds_timestamp_is_left_unchanged() {
        let seconds = 1_704_067_200_i64;
        assert_eq!(normalize_lastfm_timestamp(seconds), seconds);
    }

    #[test]
    fn zero_timestamp_is_left_unchanged() {
        assert_eq!(normalize_lastfm_timestamp(0), 0);
    }
}
