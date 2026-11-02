//! User interaction repository operations (starred, now playing, scrobbles, ratings).

use chrono::NaiveDateTime;
use diesel::prelude::*;

use crate::db::DbPool;
use crate::db::repo::error::MusicRepoError;
use crate::db::repo::music::{AlbumRow, ArtistRow, SongRow};
use crate::db::schema::{albums, artists, now_playing, scrobbles, songs, starred, user_ratings};
use crate::models::music::{Album, Artist, Song};

// ============================================================================
// Starred Repository
// ============================================================================

/// Database row representation for starred items.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = starred)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct StarredRow {
    pub id: i32,
    pub user_id: i32,
    pub artist_id: Option<i32>,
    pub album_id: Option<i32>,
    pub song_id: Option<i32>,
    pub starred_at: NaiveDateTime,
}

/// Data for inserting a new starred item.
#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = starred)]
pub struct NewStarred {
    pub user_id: i32,
    pub artist_id: Option<i32>,
    pub album_id: Option<i32>,
    pub song_id: Option<i32>,
}

/// Repository for starred items database operations.
#[derive(Clone, Debug)]
pub struct StarredRepository {
    pool: DbPool,
}

impl StarredRepository {
    /// Create a new starred repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    // ========================================================================
    // Star operations
    // ========================================================================

    /// Star an artist for a user.
    pub fn star_artist(&self, user_id: i32, artist_id: i32) -> Result<(), MusicRepoError> {
        self.star(user_id, Some(artist_id), None, None)
    }

    /// Star an album for a user.
    pub fn star_album(&self, user_id: i32, album_id: i32) -> Result<(), MusicRepoError> {
        self.star(user_id, None, Some(album_id), None)
    }

    /// Star a song for a user.
    pub fn star_song(&self, user_id: i32, song_id: i32) -> Result<(), MusicRepoError> {
        self.star(user_id, None, None, Some(song_id))
    }

    /// Insert a starred row, ignoring duplicates atomically.
    fn star(
        &self,
        user_id: i32,
        artist_id: Option<i32>,
        album_id: Option<i32>,
        song_id: Option<i32>,
    ) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;

        // Use INSERT OR IGNORE to handle race conditions atomically.
        // If the entry already exists, this is a no-op.
        let new_starred = NewStarred {
            user_id,
            artist_id,
            album_id,
            song_id,
        };

        diesel::insert_or_ignore_into(starred::table)
            .values(&new_starred)
            .execute(&mut conn)?;

        Ok(())
    }

    // ========================================================================
    // Unstar operations
    // ========================================================================

    /// Unstar an artist for a user.
    pub fn unstar_artist(&self, user_id: i32, artist_id: i32) -> Result<bool, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let deleted = diesel::delete(
            starred::table
                .filter(starred::user_id.eq(user_id))
                .filter(starred::artist_id.eq(artist_id)),
        )
        .execute(&mut conn)?;

        Ok(deleted > 0)
    }

    /// Unstar an album for a user.
    pub fn unstar_album(&self, user_id: i32, album_id: i32) -> Result<bool, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let deleted = diesel::delete(
            starred::table
                .filter(starred::user_id.eq(user_id))
                .filter(starred::album_id.eq(album_id)),
        )
        .execute(&mut conn)?;

        Ok(deleted > 0)
    }

    /// Unstar a song for a user.
    pub fn unstar_song(&self, user_id: i32, song_id: i32) -> Result<bool, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let deleted = diesel::delete(
            starred::table
                .filter(starred::user_id.eq(user_id))
                .filter(starred::song_id.eq(song_id)),
        )
        .execute(&mut conn)?;

        Ok(deleted > 0)
    }

    // ========================================================================
    // Query operations
    // ========================================================================

    /// Get all starred artists for a user with their starred timestamp.
    /// Returns (Artist, `starred_at`).
    pub fn get_starred_artists(
        &self,
        user_id: i32,
    ) -> Result<Vec<(Artist, NaiveDateTime)>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results: Vec<(StarredRow, ArtistRow)> = starred::table
            .inner_join(artists::table.on(starred::artist_id.eq(artists::id.nullable())))
            .filter(starred::user_id.eq(user_id))
            .filter(starred::artist_id.is_not_null())
            .select((StarredRow::as_select(), ArtistRow::as_select()))
            .order(starred::starred_at.desc())
            .load(&mut conn)?;

        Ok(results
            .into_iter()
            .map(|(s, a)| (Artist::from(a), s.starred_at))
            .collect())
    }

    /// Get all starred albums for a user with their starred timestamp.
    /// Returns (Album, `starred_at`).
    pub fn get_starred_albums(
        &self,
        user_id: i32,
    ) -> Result<Vec<(Album, NaiveDateTime)>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results: Vec<(StarredRow, AlbumRow)> = starred::table
            .inner_join(albums::table.on(starred::album_id.eq(albums::id.nullable())))
            .filter(starred::user_id.eq(user_id))
            .filter(starred::album_id.is_not_null())
            .select((StarredRow::as_select(), AlbumRow::as_select()))
            .order(starred::starred_at.desc())
            .load(&mut conn)?;

        Ok(results
            .into_iter()
            .map(|(s, a)| (Album::from(a), s.starred_at))
            .collect())
    }

    /// Get all starred songs for a user with their starred timestamp.
    /// Returns (Song, `starred_at`).
    pub fn get_starred_songs(
        &self,
        user_id: i32,
    ) -> Result<Vec<(Song, NaiveDateTime)>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results: Vec<(StarredRow, SongRow)> = starred::table
            .inner_join(songs::table.on(starred::song_id.eq(songs::id.nullable())))
            .filter(starred::user_id.eq(user_id))
            .filter(starred::song_id.is_not_null())
            .select((StarredRow::as_select(), SongRow::as_select()))
            .order(starred::starred_at.desc())
            .load(&mut conn)?;

        Ok(results
            .into_iter()
            .map(|(s, song)| (Song::from(song), s.starred_at))
            .collect())
    }

    /// Get starred albums for a user with pagination.
    pub fn get_starred_albums_paginated(
        &self,
        user_id: i32,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<(Album, NaiveDateTime)>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results: Vec<(StarredRow, AlbumRow)> = starred::table
            .inner_join(albums::table.on(starred::album_id.eq(albums::id.nullable())))
            .filter(starred::user_id.eq(user_id))
            .filter(starred::album_id.is_not_null())
            .filter(
                starred::album_id.eq_any(
                    super::music::album_query(&mut conn, music_folder_id)?
                        .select(albums::id.nullable()),
                ),
            )
            .select((StarredRow::as_select(), AlbumRow::as_select()))
            .order(starred::starred_at.desc())
            .offset(offset)
            .limit(limit)
            .load(&mut conn)?;

        Ok(results
            .into_iter()
            .map(|(s, a)| (Album::from(a), s.starred_at))
            .collect())
    }

    /// Get the `starred_at` timestamp for an artist.
    pub fn get_starred_at_for_artist(
        &self,
        user_id: i32,
        artist_id: i32,
    ) -> Result<Option<NaiveDateTime>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = starred::table
            .filter(starred::user_id.eq(user_id))
            .filter(starred::artist_id.eq(artist_id))
            .select(starred::starred_at)
            .first(&mut conn)
            .optional()?;

        Ok(result)
    }

    /// Get the `starred_at` timestamp for an album.
    pub fn get_starred_at_for_album(
        &self,
        user_id: i32,
        album_id: i32,
    ) -> Result<Option<NaiveDateTime>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = starred::table
            .filter(starred::user_id.eq(user_id))
            .filter(starred::album_id.eq(album_id))
            .select(starred::starred_at)
            .first(&mut conn)
            .optional()?;

        Ok(result)
    }

    /// Get the `starred_at` timestamp for a song.
    pub fn get_starred_at_for_song(
        &self,
        user_id: i32,
        song_id: i32,
    ) -> Result<Option<NaiveDateTime>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = starred::table
            .filter(starred::user_id.eq(user_id))
            .filter(starred::song_id.eq(song_id))
            .select(starred::starred_at)
            .first(&mut conn)
            .optional()?;

        Ok(result)
    }

    /// Get `starred_at` timestamps for multiple songs in a single query.
    pub fn get_starred_at_for_songs_batch(
        &self,
        user_id: i32,
        song_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, NaiveDateTime>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results: Vec<(i32, NaiveDateTime)> = starred::table
            .filter(starred::user_id.eq(user_id))
            .filter(starred::song_id.eq_any(song_ids))
            .select((starred::song_id.assume_not_null(), starred::starred_at))
            .load(&mut conn)?;

        Ok(results.into_iter().collect())
    }

    /// Get `starred_at` timestamps for multiple albums in a single query.
    pub fn get_starred_at_for_albums_batch(
        &self,
        user_id: i32,
        album_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, NaiveDateTime>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results: Vec<(i32, NaiveDateTime)> = starred::table
            .filter(starred::user_id.eq(user_id))
            .filter(starred::album_id.eq_any(album_ids))
            .select((starred::album_id.assume_not_null(), starred::starred_at))
            .load(&mut conn)?;

        Ok(results.into_iter().collect())
    }

    /// Get `starred_at` timestamps for multiple artists in a single query.
    pub fn get_starred_at_for_artists_batch(
        &self,
        user_id: i32,
        artist_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, NaiveDateTime>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results: Vec<(i32, NaiveDateTime)> = starred::table
            .filter(starred::user_id.eq(user_id))
            .filter(starred::artist_id.eq_any(artist_ids))
            .select((starred::artist_id.assume_not_null(), starred::starred_at))
            .load(&mut conn)?;

        Ok(results.into_iter().collect())
    }
}

// ============================================================================
// Now Playing Repository
// ============================================================================

/// Entry for a currently playing song.
#[derive(Debug, Clone)]
pub struct NowPlayingEntry {
    pub song: Song,
    pub username: String,
    pub player_id: Option<String>,
    pub minutes_ago: i32,
}

/// Repository for now playing database operations.
#[derive(Clone, Debug)]
pub struct NowPlayingRepository {
    pool: DbPool,
}

impl NowPlayingRepository {
    /// Create a new now playing repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Set a song as now playing for a user.
    pub fn set_now_playing(
        &self,
        user_id: i32,
        song_id: i32,
        player_id: Option<&str>,
    ) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;
        let now = chrono::Utc::now().naive_utc();

        diesel::sql_query(
            "INSERT INTO now_playing (user_id, song_id, player_id, started_at, minutes_ago)
             VALUES (?, ?, ?, ?, 0)
             ON CONFLICT(user_id) DO UPDATE SET
                song_id = excluded.song_id,
                player_id = excluded.player_id,
                started_at = excluded.started_at,
                minutes_ago = 0",
        )
        .bind::<diesel::sql_types::Integer, _>(user_id)
        .bind::<diesel::sql_types::Integer, _>(song_id)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(player_id)
        .bind::<diesel::sql_types::Timestamp, _>(now)
        .execute(&mut conn)?;

        Ok(())
    }

    /// Get all currently playing songs.
    pub fn get_all_now_playing(&self) -> Result<Vec<NowPlayingEntry>, MusicRepoError> {
        use crate::db::repo::user::UserRow;
        use crate::db::schema::users;

        let mut conn = self.pool.get()?;

        // Calculate minutes ago for each entry
        let results: Vec<(NowPlayingRow, SongRow, UserRow)> = now_playing::table
            .inner_join(songs::table.on(now_playing::song_id.eq(songs::id)))
            .inner_join(users::table.on(now_playing::user_id.eq(users::id)))
            .select((
                NowPlayingRow::as_select(),
                SongRow::as_select(),
                UserRow::as_select(),
            ))
            .load(&mut conn)?;

        Ok(results
            .into_iter()
            .map(|(np, song, user)| NowPlayingEntry {
                song: Song::from(song),
                username: user.username,
                player_id: np.player_id,
                minutes_ago: np.minutes_ago,
            })
            .collect())
    }
}

/// Database row representation for now playing.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = now_playing)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct NowPlayingRow {
    #[expect(
        dead_code,
        reason = "Selected by Diesel row mapping for table compatibility"
    )]
    id: i32,
    #[expect(
        dead_code,
        reason = "Selected by Diesel row mapping for table compatibility"
    )]
    user_id: i32,
    #[expect(
        dead_code,
        reason = "Selected by Diesel row mapping for table compatibility"
    )]
    song_id: i32,
    player_id: Option<String>,
    #[expect(
        dead_code,
        reason = "Selected by Diesel row mapping for table compatibility"
    )]
    started_at: NaiveDateTime,
    minutes_ago: i32,
}

// ============================================================================
// Scrobble Repository
// ============================================================================

/// Repository for scrobble database operations.
#[derive(Clone, Debug)]
pub struct ScrobbleRepository {
    pool: DbPool,
}

impl ScrobbleRepository {
    /// Create a new scrobble repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Record a scrobble (song play).
    ///
    /// Records a play event for `user_id` and `song_id`. If `time` is `None`,
    /// the current timestamp is used. The `submission` flag distinguishes
    /// durable scrobbles from transient now-playing notifications.
    /// Submissions also increment the song's (and its album's) play count.
    ///
    /// # Errors
    /// Returns an error if the database connection fails or the insert query fails.
    pub fn scrobble(
        &self,
        user_id: i32,
        song_id: i32,
        time: Option<i64>,
        submission: bool,
    ) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;

        // Convert time to NaiveDateTime if provided
        let played_at = time.map_or_else(
            || chrono::Utc::now().naive_utc(),
            |t| {
                chrono::DateTime::from_timestamp_millis(t)
                    .map_or_else(|| chrono::Utc::now().naive_utc(), |dt| dt.naive_utc())
            },
        );

        diesel::insert_into(scrobbles::table)
            .values((
                scrobbles::user_id.eq(user_id),
                scrobbles::song_id.eq(song_id),
                scrobbles::played_at.eq(played_at),
                scrobbles::submission.eq(submission),
            ))
            .execute(&mut conn)?;

        if submission {
            diesel::sql_query("UPDATE songs SET play_count = play_count + 1 WHERE id = ?")
                .bind::<diesel::sql_types::Integer, _>(song_id)
                .execute(&mut conn)?;

            diesel::sql_query(
                "UPDATE albums SET play_count = play_count + 1
                 WHERE id = (SELECT album_id FROM songs WHERE id = ?)",
            )
            .bind::<diesel::sql_types::Integer, _>(song_id)
            .execute(&mut conn)?;
        }

        Ok(())
    }

    /// Get recent scrobbles for a user.
    pub fn get_recent_scrobbles(
        &self,
        user_id: i32,
        limit: i64,
    ) -> Result<Vec<(Song, NaiveDateTime)>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results: Vec<(ScrobbleRow, SongRow)> = scrobbles::table
            .inner_join(songs::table.on(scrobbles::song_id.eq(songs::id)))
            .filter(scrobbles::user_id.eq(user_id))
            .filter(scrobbles::submission.eq(true))
            .select((ScrobbleRow::as_select(), SongRow::as_select()))
            .order(scrobbles::played_at.desc())
            .limit(limit)
            .load(&mut conn)?;

        Ok(results
            .into_iter()
            .map(|(scrobble, song)| (Song::from(song), scrobble.played_at))
            .collect())
    }

    /// Get the last played timestamp per song for a user, keyed by song ID.
    pub fn get_last_played_for_songs_batch(
        &self,
        user_id: i32,
        song_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, NaiveDateTime>, MusicRepoError> {
        if song_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut conn = self.pool.get()?;

        let rows: Vec<(i32, Option<NaiveDateTime>)> = scrobbles::table
            .filter(scrobbles::user_id.eq(user_id))
            .filter(scrobbles::submission.eq(true))
            .filter(scrobbles::song_id.eq_any(song_ids))
            .group_by(scrobbles::song_id)
            .select((scrobbles::song_id, diesel::dsl::max(scrobbles::played_at)))
            .load(&mut conn)?;

        Ok(rows
            .into_iter()
            .filter_map(|(id, at)| at.map(|at| (id, at)))
            .collect())
    }

    /// Get the last played timestamp per album for a user, keyed by album ID.
    pub fn get_last_played_for_albums_batch(
        &self,
        user_id: i32,
        album_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, NaiveDateTime>, MusicRepoError> {
        if album_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut conn = self.pool.get()?;

        let rows: Vec<(Option<i32>, Option<NaiveDateTime>)> = scrobbles::table
            .inner_join(songs::table.on(scrobbles::song_id.eq(songs::id)))
            .filter(scrobbles::user_id.eq(user_id))
            .filter(scrobbles::submission.eq(true))
            .filter(songs::album_id.eq_any(album_ids))
            .group_by(songs::album_id)
            .select((songs::album_id, diesel::dsl::max(scrobbles::played_at)))
            .load(&mut conn)?;

        Ok(rows.into_iter().filter_map(|(id, at)| id.zip(at)).collect())
    }
}

/// Database row representation for scrobbles.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = scrobbles)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
struct ScrobbleRow {
    #[expect(
        dead_code,
        reason = "Selected by Diesel row mapping for table compatibility"
    )]
    id: i32,
    #[expect(
        dead_code,
        reason = "Selected by Diesel row mapping for table compatibility"
    )]
    user_id: i32,
    #[expect(
        dead_code,
        reason = "Selected by Diesel row mapping for table compatibility"
    )]
    song_id: i32,
    played_at: NaiveDateTime,
    #[expect(
        dead_code,
        reason = "Selected by Diesel row mapping for table compatibility"
    )]
    submission: bool,
}

// ============================================================================
// Rating Repository
// ============================================================================

/// Group (entity ID, rating) rows into per-entity average ratings.
fn average_ratings(rows: Vec<(Option<i32>, i32)>) -> std::collections::HashMap<i32, f64> {
    let mut sums: std::collections::HashMap<i32, (f64, f64)> = std::collections::HashMap::new();
    for (id, rating) in rows {
        if let Some(id) = id {
            let entry = sums.entry(id).or_default();
            entry.0 += f64::from(rating);
            entry.1 += 1.0;
        }
    }
    sums.into_iter()
        .map(|(id, (sum, count))| (id, sum / count))
        .collect()
}

/// Repository for user rating database operations.
#[derive(Clone, Debug)]
pub struct RatingRepository {
    pool: DbPool,
}

impl RatingRepository {
    /// Create a new rating repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Set rating for a song. Rating of 0 removes the rating.
    pub fn set_song_rating(
        &self,
        user_id: i32,
        song_id: i32,
        rating: i32,
    ) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;

        if rating == 0 {
            // Remove rating
            diesel::delete(
                user_ratings::table
                    .filter(user_ratings::user_id.eq(user_id))
                    .filter(user_ratings::song_id.eq(song_id)),
            )
            .execute(&mut conn)?;
        } else {
            // Use upsert to atomically insert or update the rating
            diesel::sql_query(
                "INSERT INTO user_ratings (user_id, song_id, rating, updated_at)
                 VALUES (?, ?, ?, CURRENT_TIMESTAMP)
                 ON CONFLICT (user_id, song_id) WHERE song_id IS NOT NULL
                 DO UPDATE SET rating = excluded.rating, updated_at = CURRENT_TIMESTAMP",
            )
            .bind::<diesel::sql_types::Integer, _>(user_id)
            .bind::<diesel::sql_types::Integer, _>(song_id)
            .bind::<diesel::sql_types::Integer, _>(rating)
            .execute(&mut conn)?;
        }

        Ok(())
    }

    /// Set rating for an album. Rating of 0 removes the rating.
    pub fn set_album_rating(
        &self,
        user_id: i32,
        album_id: i32,
        rating: i32,
    ) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;

        if rating == 0 {
            diesel::delete(
                user_ratings::table
                    .filter(user_ratings::user_id.eq(user_id))
                    .filter(user_ratings::album_id.eq(album_id)),
            )
            .execute(&mut conn)?;
        } else {
            diesel::sql_query(
                "INSERT INTO user_ratings (user_id, album_id, rating, updated_at)
                 VALUES (?, ?, ?, CURRENT_TIMESTAMP)
                 ON CONFLICT (user_id, album_id) WHERE album_id IS NOT NULL
                 DO UPDATE SET rating = excluded.rating, updated_at = CURRENT_TIMESTAMP",
            )
            .bind::<diesel::sql_types::Integer, _>(user_id)
            .bind::<diesel::sql_types::Integer, _>(album_id)
            .bind::<diesel::sql_types::Integer, _>(rating)
            .execute(&mut conn)?;
        }

        Ok(())
    }

    /// Set rating for an artist. Rating of 0 removes the rating.
    pub fn set_artist_rating(
        &self,
        user_id: i32,
        artist_id: i32,
        rating: i32,
    ) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;

        if rating == 0 {
            diesel::delete(
                user_ratings::table
                    .filter(user_ratings::user_id.eq(user_id))
                    .filter(user_ratings::artist_id.eq(artist_id)),
            )
            .execute(&mut conn)?;
        } else {
            diesel::sql_query(
                "INSERT INTO user_ratings (user_id, artist_id, rating, updated_at)
                 VALUES (?, ?, ?, CURRENT_TIMESTAMP)
                 ON CONFLICT (user_id, artist_id) WHERE artist_id IS NOT NULL
                 DO UPDATE SET rating = excluded.rating, updated_at = CURRENT_TIMESTAMP",
            )
            .bind::<diesel::sql_types::Integer, _>(user_id)
            .bind::<diesel::sql_types::Integer, _>(artist_id)
            .bind::<diesel::sql_types::Integer, _>(rating)
            .execute(&mut conn)?;
        }

        Ok(())
    }

    /// Get rating for a song.
    pub fn get_song_rating(
        &self,
        user_id: i32,
        song_id: i32,
    ) -> Result<Option<i32>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = user_ratings::table
            .filter(user_ratings::user_id.eq(user_id))
            .filter(user_ratings::song_id.eq(song_id))
            .select(user_ratings::rating)
            .first(&mut conn)
            .optional()?;

        Ok(result)
    }

    /// Get rating for an album.
    pub fn get_album_rating(
        &self,
        user_id: i32,
        album_id: i32,
    ) -> Result<Option<i32>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = user_ratings::table
            .filter(user_ratings::user_id.eq(user_id))
            .filter(user_ratings::album_id.eq(album_id))
            .select(user_ratings::rating)
            .first(&mut conn)
            .optional()?;

        Ok(result)
    }

    /// Get rating for an artist.
    pub fn get_artist_rating(
        &self,
        user_id: i32,
        artist_id: i32,
    ) -> Result<Option<i32>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = user_ratings::table
            .filter(user_ratings::user_id.eq(user_id))
            .filter(user_ratings::artist_id.eq(artist_id))
            .select(user_ratings::rating)
            .first(&mut conn)
            .optional()?;

        Ok(result)
    }

    /// Get ratings for a set of songs for a user, keyed by song ID.
    pub fn get_song_ratings_batch(
        &self,
        user_id: i32,
        song_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, i32>, MusicRepoError> {
        if song_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut conn = self.pool.get()?;

        let rows: Vec<(Option<i32>, i32)> = user_ratings::table
            .filter(user_ratings::user_id.eq(user_id))
            .filter(user_ratings::song_id.eq_any(song_ids))
            .select((user_ratings::song_id, user_ratings::rating))
            .load(&mut conn)?;

        Ok(rows
            .into_iter()
            .filter_map(|(id, rating)| id.map(|id| (id, rating)))
            .collect())
    }

    /// Get ratings for a set of albums for a user, keyed by album ID.
    pub fn get_album_ratings_batch(
        &self,
        user_id: i32,
        album_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, i32>, MusicRepoError> {
        if album_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut conn = self.pool.get()?;

        let rows: Vec<(Option<i32>, i32)> = user_ratings::table
            .filter(user_ratings::user_id.eq(user_id))
            .filter(user_ratings::album_id.eq_any(album_ids))
            .select((user_ratings::album_id, user_ratings::rating))
            .load(&mut conn)?;

        Ok(rows
            .into_iter()
            .filter_map(|(id, rating)| id.map(|id| (id, rating)))
            .collect())
    }

    /// Get average ratings across all users for a set of songs, keyed by song ID.
    pub fn get_average_song_ratings_batch(
        &self,
        song_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, f64>, MusicRepoError> {
        if song_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut conn = self.pool.get()?;

        let rows: Vec<(Option<i32>, i32)> = user_ratings::table
            .filter(user_ratings::song_id.eq_any(song_ids))
            .select((user_ratings::song_id, user_ratings::rating))
            .load(&mut conn)?;

        Ok(average_ratings(rows))
    }

    /// Get average ratings across all users for a set of albums, keyed by album ID.
    pub fn get_average_album_ratings_batch(
        &self,
        album_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, f64>, MusicRepoError> {
        if album_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut conn = self.pool.get()?;

        let rows: Vec<(Option<i32>, i32)> = user_ratings::table
            .filter(user_ratings::album_id.eq_any(album_ids))
            .select((user_ratings::album_id, user_ratings::rating))
            .load(&mut conn)?;

        Ok(average_ratings(rows))
    }

    /// Get highest rated albums for a user with pagination.
    /// Returns album IDs ordered by rating descending, then by album name.
    pub fn get_highest_rated_album_ids(
        &self,
        user_id: i32,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<i32>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results: Vec<i32> = user_ratings::table
            .filter(user_ratings::user_id.eq(user_id))
            .filter(user_ratings::album_id.is_not_null())
            .filter(
                user_ratings::album_id.eq_any(
                    super::music::album_query(&mut conn, music_folder_id)?
                        .select(albums::id.nullable()),
                ),
            )
            .filter(user_ratings::rating.gt(0))
            .select(user_ratings::album_id)
            .order(user_ratings::rating.desc())
            .offset(offset)
            .limit(limit)
            .load::<Option<i32>>(&mut conn)?
            .into_iter()
            .flatten()
            .collect();

        Ok(results)
    }
}
