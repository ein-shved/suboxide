//! Bookmark repository operations.

use chrono::NaiveDateTime;
use diesel::prelude::*;

use crate::db::DbPool;
use crate::db::repo::error::MusicRepoError;
use crate::db::repo::music::SongRow;
use crate::db::schema::{bookmarks, songs};
use crate::models::music::Song;

/// A bookmark joined with its song.
#[derive(Debug, Clone)]
pub struct BookmarkEntry {
    pub song: Song,
    pub position: i64,
    pub comment: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Database row representation for bookmarks.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = bookmarks)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct BookmarkRow {
    pub id: i32,
    pub user_id: i32,
    pub song_id: i32,
    pub position: i64,
    pub comment: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Repository for bookmark database operations.
#[derive(Clone, Debug)]
pub struct BookmarkRepository {
    pool: DbPool,
}

impl BookmarkRepository {
    /// Create a new bookmark repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Get all bookmarks for a user, joined with their songs.
    pub fn get_for_user(&self, user_id: i32) -> Result<Vec<BookmarkEntry>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let rows: Vec<(BookmarkRow, SongRow)> = bookmarks::table
            .inner_join(songs::table.on(bookmarks::song_id.eq(songs::id)))
            .filter(bookmarks::user_id.eq(user_id))
            .select((BookmarkRow::as_select(), SongRow::as_select()))
            .order(bookmarks::updated_at.desc())
            .load(&mut conn)?;

        Ok(rows
            .into_iter()
            .map(|(bookmark, song)| BookmarkEntry {
                song: Song::from(song),
                position: bookmark.position,
                comment: bookmark.comment,
                created_at: bookmark.created_at,
                updated_at: bookmark.updated_at,
            })
            .collect())
    }

    /// Create or update the bookmark position for a song.
    pub fn upsert(
        &self,
        user_id: i32,
        song_id: i32,
        position: i64,
        comment: Option<&str>,
    ) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;

        diesel::sql_query(
            "INSERT INTO bookmarks (user_id, song_id, position, comment)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (user_id, song_id)
             DO UPDATE SET position = excluded.position,
                           comment = COALESCE(excluded.comment, bookmarks.comment),
                           updated_at = CURRENT_TIMESTAMP",
        )
        .bind::<diesel::sql_types::Integer, _>(user_id)
        .bind::<diesel::sql_types::Integer, _>(song_id)
        .bind::<diesel::sql_types::BigInt, _>(position)
        .bind::<diesel::sql_types::Nullable<diesel::sql_types::Text>, _>(comment)
        .execute(&mut conn)?;

        Ok(())
    }

    /// Delete the bookmark for a song. Returns whether a bookmark existed.
    pub fn delete(&self, user_id: i32, song_id: i32) -> Result<bool, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let deleted = diesel::delete(
            bookmarks::table
                .filter(bookmarks::user_id.eq(user_id))
                .filter(bookmarks::song_id.eq(song_id)),
        )
        .execute(&mut conn)?;

        Ok(deleted > 0)
    }

    /// Get bookmark positions for a set of songs, keyed by song ID.
    pub fn get_positions_for_songs(
        &self,
        user_id: i32,
        song_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, i64>, MusicRepoError> {
        if song_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let mut conn = self.pool.get()?;

        let rows: Vec<(i32, i64)> = bookmarks::table
            .filter(bookmarks::user_id.eq(user_id))
            .filter(bookmarks::song_id.eq_any(song_ids))
            .select((bookmarks::song_id, bookmarks::position))
            .load(&mut conn)?;

        Ok(rows.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::BookmarkRepository;
    use crate::db::schema::{music_folders, songs, users};
    use crate::db::{DbConfig, DbPool, initialize_database};
    use diesel::prelude::*;

    fn test_pool() -> DbPool {
        let config = DbConfig {
            database_url: ":memory:".to_string(),
            max_connections: 1,
            ..DbConfig::default()
        };
        let pool = config.build_pool().expect("pool must build");
        initialize_database(&mut pool.get().expect("connection must checkout"))
            .expect("schema must initialize");
        pool
    }

    fn seed_user_and_song(pool: &DbPool, username: &str) -> (i32, i32) {
        let mut conn = pool.get().expect("connection must checkout");

        diesel::insert_into(users::table)
            .values((
                users::username.eq(username),
                users::password_hash.eq("hash"),
            ))
            .execute(&mut conn)
            .expect("user must insert");
        let user_id: i32 = users::table
            .filter(users::username.eq(username))
            .select(users::id)
            .first(&mut conn)
            .expect("user must exist");

        diesel::insert_into(music_folders::table)
            .values((
                music_folders::name.eq("music"),
                music_folders::path.eq(format!("/music/{username}")),
            ))
            .execute(&mut conn)
            .expect("folder must insert");
        let folder_id: i32 = music_folders::table
            .select(music_folders::id)
            .first(&mut conn)
            .expect("folder must exist");

        diesel::insert_into(songs::table)
            .values((
                songs::title.eq("song"),
                songs::music_folder_id.eq(folder_id),
                songs::path.eq(format!("/music/{username}/song.flac")),
                songs::parent_path.eq(format!("/music/{username}")),
                songs::content_type.eq("audio/flac"),
                songs::suffix.eq("flac"),
            ))
            .execute(&mut conn)
            .expect("song must insert");
        let song_id: i32 = songs::table
            .select(songs::id)
            .first(&mut conn)
            .expect("song must exist");

        (user_id, song_id)
    }

    #[test]
    fn upsert_creates_then_updates_position() {
        let pool = test_pool();
        let (user_id, song_id) = seed_user_and_song(&pool, "alice");
        let repo = BookmarkRepository::new(pool);

        repo.upsert(user_id, song_id, 1_000, Some("intro"))
            .expect("upsert must succeed");
        repo.upsert(user_id, song_id, 2_000, None)
            .expect("second upsert must succeed");

        let entries = repo.get_for_user(user_id).expect("bookmarks must load");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].position, 2_000);
        // Comment omitted in the update keeps the previous value
        assert_eq!(entries[0].comment.as_deref(), Some("intro"));
        assert_eq!(entries[0].song.id, song_id);
    }

    #[test]
    fn bookmarks_are_per_user() {
        let pool = test_pool();
        let (alice_id, song_id) = seed_user_and_song(&pool, "alice");
        let (bob_id, _) = seed_user_and_song(&pool, "bob");
        let repo = BookmarkRepository::new(pool);

        repo.upsert(alice_id, song_id, 500, None)
            .expect("upsert must succeed");

        assert_eq!(
            repo.get_for_user(alice_id).expect("load").len(),
            1,
            "alice sees her bookmark"
        );
        assert!(
            repo.get_for_user(bob_id).expect("load").is_empty(),
            "bob does not see alice's bookmark"
        );
    }

    #[test]
    fn delete_removes_existing_bookmark_only() {
        let pool = test_pool();
        let (user_id, song_id) = seed_user_and_song(&pool, "alice");
        let repo = BookmarkRepository::new(pool);

        assert!(!repo.delete(user_id, song_id).expect("delete"));

        repo.upsert(user_id, song_id, 500, None)
            .expect("upsert must succeed");
        assert!(repo.delete(user_id, song_id).expect("delete"));
        assert!(repo.get_for_user(user_id).expect("load").is_empty());
    }

    #[test]
    fn get_positions_for_songs_returns_positions_by_song_id() {
        let pool = test_pool();
        let (user_id, song_id) = seed_user_and_song(&pool, "alice");
        let repo = BookmarkRepository::new(pool);

        repo.upsert(user_id, song_id, 42_000, None)
            .expect("upsert must succeed");

        let positions = repo
            .get_positions_for_songs(user_id, &[song_id, song_id + 100])
            .expect("positions must load");
        assert_eq!(positions.get(&song_id), Some(&42_000));
        assert_eq!(positions.len(), 1);
    }
}
