//! Music library repository operations.

use chrono::NaiveDateTime;
use diesel::prelude::*;

use crate::db::DbPool;
use crate::db::repo::error::{MusicRepoError, MusicRepoErrorKind};
use crate::db::schema::{albums, artists, music_folders, songs};
use crate::models::music::{Album, Artist, MusicFolder, NewMusicFolder, Song};

// ============================================================================
// MusicFolder Repository
// ============================================================================

/// Database row representation for music folders.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = music_folders)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct MusicFolderRow {
    pub id: i32,
    pub name: String,
    pub path: String,
    pub parent: Option<i32>,
    pub enabled: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl From<MusicFolderRow> for MusicFolder {
    fn from(row: MusicFolderRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            path: row.path,
            parent: row.parent,
            enabled: row.enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Repository for music folder database operations.
#[derive(Clone, Debug)]
pub struct MusicFolderRepository {
    pool: DbPool,
}

impl MusicFolderRepository {
    /// Create a new music folder repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Get all music folders.
    pub fn find_all(&self) -> Result<Vec<MusicFolder>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = music_folders::table
            .filter(music_folders::parent.is_null())
            .select(MusicFolderRow::as_select())
            .order(music_folders::name.asc())
            .load(&mut conn)?;

        Ok(results.into_iter().map(MusicFolder::from).collect())
    }

    /// Get all enabled music folders.
    pub fn find_enabled(&self) -> Result<Vec<MusicFolder>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = music_folders::table
            .filter(music_folders::parent.is_null())
            .filter(music_folders::enabled.eq(true))
            .select(MusicFolderRow::as_select())
            .order(music_folders::name.asc())
            .load(&mut conn)?;

        Ok(results.into_iter().map(MusicFolder::from).collect())
    }

    /// Find a music folder by ID.
    pub fn find_by_id(&self, folder_id: i32) -> Result<Option<MusicFolder>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = music_folders::table
            .filter(music_folders::id.eq(folder_id))
            .select(MusicFolderRow::as_select())
            .first(&mut conn)
            .optional()?;

        Ok(result.map(MusicFolder::from))
    }

    /// Find a music folder by path.
    pub fn find_by_path(&self, path: &str) -> Result<Option<MusicFolder>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = music_folders::table
            .filter(music_folders::path.eq(path))
            .select(MusicFolderRow::as_select())
            .first(&mut conn)
            .optional()?;

        Ok(result.map(MusicFolder::from))
    }

    /// Create a new music folder.
    pub fn create(&self, new_folder: &NewMusicFolder) -> Result<MusicFolder, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let canonical = std::fs::canonicalize(&new_folder.path).map_err(|error| {
            MusicRepoError::with_source(MusicRepoErrorKind::InvalidInput, &new_folder.path, error)
        })?;
        if !canonical.is_dir() {
            return Err(MusicRepoError::new(
                MusicRepoErrorKind::InvalidInput,
                "root must be a directory",
            ));
        }
        let path = canonical.to_str().ok_or_else(|| {
            MusicRepoError::new(MusicRepoErrorKind::InvalidInput, "root path is not UTF-8")
        })?;
        conn.transaction(|conn| {
            let roots = music_folders::table
                .filter(music_folders::parent.is_null())
                .select(music_folders::path)
                .load::<String>(conn)?;
            if roots.iter().any(|root| {
                canonical.starts_with(root) || std::path::Path::new(root).starts_with(&canonical)
            }) {
                return Err(MusicRepoError::new(
                    MusicRepoErrorKind::AlreadyExists,
                    "registered roots overlap",
                ));
            }
            diesel::insert_into(music_folders::table)
                .values((
                    music_folders::name.eq(&new_folder.name),
                    music_folders::path.eq(path),
                    music_folders::enabled.eq(new_folder.enabled),
                ))
                .execute(conn)?;
            let folder = music_folders::table
                .filter(music_folders::path.eq(path))
                .select(MusicFolderRow::as_select())
                .first::<MusicFolderRow>(conn)?;
            Ok(folder.into())
        })
    }

    /// Remove a registered root, its descendants, and their indexed songs.
    pub fn delete(&self, folder_id: i32) -> Result<bool, MusicRepoError> {
        let mut conn = self.pool.get()?;
        conn.transaction(|conn| {
            let ids = subtree_ids(conn, folder_id)?;
            if ids.is_empty() {
                return Ok(false);
            }
            let albums = songs::table
                .filter(songs::music_folder_id.eq_any(&ids))
                .select(songs::album_id)
                .load::<Option<i32>>(conn)?
                .into_iter()
                .flatten()
                .collect();
            diesel::delete(songs::table.filter(songs::music_folder_id.eq_any(&ids)))
                .execute(conn)?;
            diesel::delete(music_folders::table.find(folder_id)).execute(conn)?;
            reconcile_albums(conn, &albums)?;
            cleanup_cue_sources(conn)?;
            Ok(true)
        })
    }

    /// Enable or disable a music folder.
    pub fn set_enabled(&self, folder_id: i32, enabled: bool) -> Result<bool, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let updated = diesel::update(
            music_folders::table
                .filter(music_folders::id.eq(folder_id))
                .filter(music_folders::parent.is_null()),
        )
        .set(music_folders::enabled.eq(enabled))
        .execute(&mut conn)?;

        Ok(updated > 0)
    }
}

// ============================================================================
// Artist Repository
// ============================================================================

/// Normalize a directory selector without allowing it to escape its root.
pub fn normalize_directory_path(path: &std::path::Path) -> Result<String, MusicRepoError> {
    use std::path::Component;
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => normalized.push(name),
            Component::CurDir => {}
            _ => {
                return Err(MusicRepoError::new(
                    MusicRepoErrorKind::InvalidInput,
                    "directory must be root-relative and contain no '..'",
                ));
            }
        }
    }
    normalized.to_str().map(str::to_owned).ok_or_else(|| {
        MusicRepoError::new(
            MusicRepoErrorKind::InvalidInput,
            "directory path is not UTF-8",
        )
    })
}

/// Insert containing folders and ancestors relative to a registered root.
pub(crate) fn ensure_directory(
    conn: &mut SqliteConnection,
    root_id: i32,
    path: &str,
) -> Result<MusicFolder, diesel::result::Error> {
    let mut folder = music_folders::table
        .find(root_id)
        .filter(music_folders::parent.is_null())
        .select(MusicFolderRow::as_select())
        .first::<MusicFolderRow>(conn)?;
    for component in std::path::Path::new(path).components() {
        let absolute = std::path::Path::new(&folder.path).join(component);
        let absolute = absolute.to_string_lossy();
        let name = component.as_os_str().to_string_lossy();
        diesel::insert_into(music_folders::table)
            .values((
                music_folders::path.eq(absolute.as_ref()),
                music_folders::name.eq(name.as_ref()),
                music_folders::parent.eq(folder.id),
            ))
            .on_conflict(music_folders::path)
            .do_nothing()
            .execute(conn)?;
        folder = music_folders::table
            .filter(music_folders::path.eq(absolute.as_ref()))
            .select(MusicFolderRow::as_select())
            .first(conn)?;
    }
    Ok(folder.into())
}

#[derive(QueryableByName)]
struct FolderId {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
}

/// Refresh affected albums from all persisted members, including disabled roots.
pub(crate) fn reconcile_albums(
    conn: &mut diesel::SqliteConnection,
    dirty_album_ids: &std::collections::HashSet<i32>,
) -> Result<(), diesel::result::Error> {
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

    diesel::sql_query(sql).execute(conn)?;

    diesel::sql_query(format!("DELETE FROM albums WHERE id IN ({ids}) AND NOT EXISTS (SELECT 1 FROM songs WHERE album_id = albums.id)")).execute(conn)?;
    Ok(())
}

/// IDs in a folder subtree; public filters must start at a registered root.
pub(crate) fn subtree_ids(
    conn: &mut SqliteConnection,
    folder_id: i32,
) -> Result<Vec<i32>, diesel::result::Error> {
    let query = "WITH RECURSIVE tree(id) AS (SELECT id FROM music_folders WHERE id = ? AND parent IS NULL UNION ALL SELECT f.id FROM music_folders f JOIN tree t ON f.parent = t.id) SELECT id FROM tree";
    Ok(diesel::sql_query(query)
        .bind::<diesel::sql_types::Integer, _>(folder_id)
        .load::<FolderId>(conn)?
        .into_iter()
        .map(|row| row.id)
        .collect())
}

impl MusicFolderRepository {
    pub fn ensure(&self, root_id: i32, path: &str) -> Result<MusicFolder, MusicRepoError> {
        let path = normalize_directory_path(std::path::Path::new(path))?;
        let mut conn = self.pool.get()?;
        Ok(conn.transaction(|conn| ensure_directory(conn, root_id, &path))?)
    }

    pub fn find(&self, root_id: i32, path: &str) -> Result<Option<MusicFolder>, MusicRepoError> {
        let path = normalize_directory_path(std::path::Path::new(path))?;
        let Some(root) = self
            .find_by_id(root_id)?
            .filter(|root| root.parent.is_none())
        else {
            return Ok(None);
        };
        if path.is_empty() {
            return Ok(Some(root));
        }
        let absolute = std::path::Path::new(&root.path).join(path);
        self.find_by_path(&absolute.to_string_lossy())
    }

    pub fn find_by_root(&self, root_id: i32) -> Result<Vec<MusicFolder>, MusicRepoError> {
        let mut conn = self.pool.get()?;
        let ids = subtree_ids(&mut conn, root_id)?;
        Ok(music_folders::table
            .filter(music_folders::id.eq_any(ids))
            .order(music_folders::path.asc())
            .select(MusicFolderRow::as_select())
            .load::<MusicFolderRow>(&mut conn)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn children(&self, folder_id: i32) -> Result<Vec<MusicFolder>, MusicRepoError> {
        let mut conn = self.pool.get()?;
        Ok(music_folders::table
            .filter(music_folders::parent.eq(folder_id))
            .select(MusicFolderRow::as_select())
            .load::<MusicFolderRow>(&mut conn)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// Remove a confirmed empty leaf; recursive discovery prunes ancestors afterwards.
    pub fn prune(&self, folder_id: i32) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;
        diesel::sql_query("DELETE FROM music_folders WHERE id = ? AND parent IS NOT NULL AND NOT EXISTS (SELECT 1 FROM songs WHERE music_folder_id = music_folders.id) AND id NOT IN (SELECT parent FROM music_folders WHERE parent IS NOT NULL)")
            .bind::<diesel::sql_types::Integer, _>(folder_id).execute(&mut conn)?;
        Ok(())
    }
}

/// Database row representation for artists.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = artists)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct ArtistRow {
    pub id: i32,
    pub name: String,
    pub sort_name: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub cover_art: Option<String>,
    pub artist_image_url: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl From<ArtistRow> for Artist {
    fn from(row: ArtistRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            sort_name: row.sort_name,
            musicbrainz_id: row.musicbrainz_id,
            cover_art: row.cover_art,
            artist_image_url: row.artist_image_url,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Repository for artist database operations.
#[derive(Clone, Debug)]
pub struct ArtistRepository {
    pool: DbPool,
}

impl ArtistRepository {
    /// Create a new artist repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Get all artists.
    pub fn find_all(&self) -> Result<Vec<Artist>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = artists::table
            .select(ArtistRow::as_select())
            .order(artists::name.asc())
            .load(&mut conn)?;

        Ok(results.into_iter().map(Artist::from).collect())
    }

    /// Get artists with songs in a music folder.
    pub fn find_by_music_folder(&self, folder_id: i32) -> Result<Vec<Artist>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = artists::table
            .inner_join(songs::table.on(songs::artist_id.eq(artists::id.nullable())))
            .filter(songs::music_folder_id.eq_any(subtree_ids(&mut conn, folder_id)?))
            .select(ArtistRow::as_select())
            .distinct()
            .order(artists::name.asc())
            .load(&mut conn)?;

        Ok(results.into_iter().map(Artist::from).collect())
    }

    /// Find an artist by ID.
    pub fn find_by_id(&self, artist_id: i32) -> Result<Option<Artist>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = artists::table
            .filter(artists::id.eq(artist_id))
            .select(ArtistRow::as_select())
            .first(&mut conn)
            .optional()?;

        Ok(result.map(Artist::from))
    }

    /// Find an artist by name.
    pub fn find_by_name(&self, name: &str) -> Result<Option<Artist>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = artists::table
            .filter(artists::name.eq(name))
            .select(ArtistRow::as_select())
            .first(&mut conn)
            .optional()?;

        Ok(result.map(Artist::from))
    }

    /// Update an artist's cover art ID.
    pub fn update_cover_art(
        &self,
        artist_id: i32,
        cover_art_id: Option<&str>,
    ) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;

        diesel::update(artists::table.filter(artists::id.eq(artist_id)))
            .set(artists::cover_art.eq(cover_art_id))
            .execute(&mut conn)?;

        Ok(())
    }

    /// Count albums for an artist.
    pub fn count_albums(&self, artist_id: i32) -> Result<i64, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let count = albums::table
            .filter(albums::artist_id.eq(artist_id))
            .count()
            .get_result(&mut conn)?;

        Ok(count)
    }

    /// Get the most recent update time for any artist.
    pub fn get_last_modified(&self) -> Result<Option<NaiveDateTime>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = artists::table
            .select(diesel::dsl::max(artists::updated_at))
            .first(&mut conn)?;

        Ok(result)
    }

    /// Count albums for multiple artists in a single query.
    /// Returns a `HashMap` mapping `artist_id` to `album_count`.
    pub fn count_albums_batch(
        &self,
        artist_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, i64>, MusicRepoError> {
        use std::collections::HashMap;

        if artist_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut conn = self.pool.get()?;

        let counts: Vec<(i32, i64)> = albums::table
            .filter(albums::artist_id.eq_any(artist_ids))
            .group_by(albums::artist_id)
            .select((
                albums::artist_id.assume_not_null(),
                diesel::dsl::count_star(),
            ))
            .load(&mut conn)?;

        Ok(counts.into_iter().collect())
    }

    /// Search artists by folded name with pagination.
    /// An empty query returns all artists. When `music_folder_id` is set,
    /// only artists with songs in that folder are returned.
    pub fn search(
        &self,
        query: &str,
        music_folder_id: Option<i32>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Artist>, MusicRepoError> {
        let mut conn = self.pool.get()?;
        let mut db_query = artists::table
            .select(ArtistRow::as_select())
            .order(artists::name.asc())
            .offset(offset)
            .limit(limit)
            .into_boxed();

        if !query.is_empty() {
            let pattern = search_pattern(query);
            db_query = db_query.filter(artists::search_name.like(pattern));
        }

        if let Some(folder_id) = music_folder_id {
            db_query = db_query.filter(diesel::dsl::exists(
                songs::table
                    .filter(songs::music_folder_id.eq_any(subtree_ids(&mut conn, folder_id)?))
                    .filter(songs::artist_id.eq(artists::id.nullable())),
            ));
        }

        let results = db_query.load(&mut conn)?;
        Ok(results.into_iter().map(Artist::from).collect())
    }
}

// ============================================================================
// Album Repository
// ============================================================================

/// Build the SQL `LIKE` pattern for a search query, folding case and accents
/// the same way search names are stored.
fn search_pattern(query: &str) -> String {
    format!("%{}%", crate::models::music::normalize_search_text(query))
}

/// Database row representation for albums.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = albums)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct AlbumRow {
    pub id: i32,
    pub name: String,
    pub sort_name: Option<String>,
    pub artist_id: Option<i32>,
    pub artist_name: Option<String>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub cover_art: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub duration: i32,
    pub song_count: i32,
    pub play_count: i32,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl From<AlbumRow> for Album {
    fn from(row: AlbumRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            sort_name: row.sort_name,
            artist_id: row.artist_id,
            artist_name: row.artist_name,
            year: row.year,
            genre: row.genre,
            cover_art: row.cover_art,
            musicbrainz_id: row.musicbrainz_id,
            duration: row.duration,
            song_count: row.song_count,
            play_count: row.play_count,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Apply root membership before list ordering and pagination.
pub(crate) fn album_query(
    conn: &mut SqliteConnection,
    root: Option<i32>,
) -> Result<albums::BoxedQuery<'static, diesel::sqlite::Sqlite>, diesel::result::Error> {
    let mut query = albums::table.into_boxed();
    if let Some(root) = root {
        let ids = subtree_ids(conn, root)?;
        query = query.filter(
            albums::id.nullable().eq_any(
                songs::table
                    .filter(songs::music_folder_id.eq_any(ids))
                    .select(songs::album_id),
            ),
        );
    }
    Ok(query)
}

/// Repository for album database operations.
#[derive(Clone, Debug)]
pub struct AlbumRepository {
    pool: DbPool,
}

impl AlbumRepository {
    /// Create a new album repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Get all albums ordered by name.
    pub fn find_all(&self) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = albums::table
            .select(AlbumRow::as_select())
            .order(albums::name.asc())
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find an album by ID.
    pub fn find_by_id(&self, album_id: i32) -> Result<Option<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = albums::table
            .filter(albums::id.eq(album_id))
            .select(AlbumRow::as_select())
            .first(&mut conn)
            .optional()?;

        Ok(result.map(Album::from))
    }

    /// Find albums by artist ID.
    pub fn find_by_artist(&self, artist_id: i32) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = albums::table
            .filter(albums::artist_id.eq(artist_id))
            .select(AlbumRow::as_select())
            .order(albums::year.asc())
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find albums ordered alphabetically by name with pagination.
    pub fn find_alphabetical_by_name(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = album_query(&mut conn, music_folder_id)?
            .select(AlbumRow::as_select())
            .order(albums::name.asc())
            .offset(offset)
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find albums ordered alphabetically by artist name with pagination.
    pub fn find_alphabetical_by_artist(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = album_query(&mut conn, music_folder_id)?
            .select(AlbumRow::as_select())
            .order((albums::artist_name.asc(), albums::name.asc()))
            .offset(offset)
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find newest albums (by `created_at`) with pagination.
    pub fn find_newest(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = album_query(&mut conn, music_folder_id)?
            .select(AlbumRow::as_select())
            .order(albums::created_at.desc())
            .offset(offset)
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find most frequently played albums with pagination.
    pub fn find_frequent(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = album_query(&mut conn, music_folder_id)?
            .select(AlbumRow::as_select())
            .order(albums::play_count.desc())
            .offset(offset)
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find recently played albums with pagination.
    /// Note: Using `updated_at` as a proxy for last played time.
    pub fn find_recent(
        &self,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = album_query(&mut conn, music_folder_id)?
            .filter(albums::play_count.gt(0))
            .select(AlbumRow::as_select())
            .order(albums::updated_at.desc())
            .offset(offset)
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find random albums.
    pub fn find_random(
        &self,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        // SQLite uses RANDOM() for random ordering
        let results = album_query(&mut conn, music_folder_id)?
            .select(AlbumRow::as_select())
            .order(diesel::dsl::sql::<diesel::sql_types::Integer>("RANDOM()"))
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find albums by year range with pagination.
    pub fn find_by_year_range(
        &self,
        from_year: i32,
        to_year: i32,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = album_query(&mut conn, music_folder_id)?
            .filter(albums::year.ge(from_year))
            .filter(albums::year.le(to_year))
            .select(AlbumRow::as_select())
            .order(albums::year.asc())
            .offset(offset)
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find albums by genre with pagination.
    pub fn find_by_genre(
        &self,
        genre: &str,
        offset: i64,
        limit: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = album_query(&mut conn, music_folder_id)?
            .filter(albums::genre.eq(genre))
            .select(AlbumRow::as_select())
            .order(albums::name.asc())
            .offset(offset)
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Search albums by folded name with pagination.
    /// An empty query returns all albums. When `music_folder_id` is set,
    /// only albums with songs in that folder are returned.
    pub fn search(
        &self,
        query: &str,
        music_folder_id: Option<i32>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let mut db_query = albums::table
            .select(AlbumRow::as_select())
            .order(albums::name.asc())
            .offset(offset)
            .limit(limit)
            .into_boxed();

        if !query.is_empty() {
            let pattern = search_pattern(query);
            db_query = db_query.filter(albums::search_name.like(pattern));
        }

        if let Some(folder_id) = music_folder_id {
            db_query = db_query.filter(diesel::dsl::exists(
                songs::table
                    .filter(songs::music_folder_id.eq_any(subtree_ids(&mut conn, folder_id)?))
                    .filter(songs::album_id.eq(albums::id.nullable())),
            ));
        }

        let results = db_query.load(&mut conn)?;
        Ok(results.into_iter().map(Album::from).collect())
    }

    /// Find albums by IDs.
    pub fn find_by_ids(&self, album_ids: &[i32]) -> Result<Vec<Album>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = albums::table
            .filter(albums::id.eq_any(album_ids))
            .select(AlbumRow::as_select())
            .load(&mut conn)?;

        Ok(results.into_iter().map(Album::from).collect())
    }
}

// ============================================================================
// Song Repository
// ============================================================================

/// Database row representation for songs.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = songs)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct SongRow {
    pub id: i32,
    pub title: String,
    pub sort_name: Option<String>,
    pub album_id: Option<i32>,
    pub artist_id: Option<i32>,
    pub artist_name: Option<String>,
    pub album_name: Option<String>,
    pub music_folder_id: i32,
    pub path: String,
    pub cue_track_id: Option<i32>,
    pub parent_path: String,
    pub file_size: i64,
    pub content_type: String,
    pub suffix: String,
    pub duration: i32,
    pub bit_rate: Option<i32>,
    pub bit_depth: Option<i32>,
    pub sampling_rate: Option<i32>,
    pub channel_count: Option<i32>,
    pub track_number: Option<i32>,
    pub disc_number: Option<i32>,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub cover_art: Option<String>,
    pub musicbrainz_id: Option<String>,
    pub play_count: i32,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

impl From<SongRow> for Song {
    fn from(row: SongRow) -> Self {
        Self {
            id: row.id,
            title: row.title,
            sort_name: row.sort_name,
            album_id: row.album_id,
            artist_id: row.artist_id,
            artist_name: row.artist_name,
            album_name: row.album_name,
            music_folder_id: row.music_folder_id,
            path: row.path,
            cue_track_id: row.cue_track_id,
            parent_path: row.parent_path,
            file_size: row.file_size,
            content_type: row.content_type,
            suffix: row.suffix,
            duration: row.duration,
            bit_rate: row.bit_rate,
            bit_depth: row.bit_depth,
            sampling_rate: row.sampling_rate,
            channel_count: row.channel_count,
            track_number: row.track_number,
            disc_number: row.disc_number,
            year: row.year,
            genre: row.genre,
            cover_art: row.cover_art,
            musicbrainz_id: row.musicbrainz_id,
            play_count: row.play_count,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

#[derive(Debug)]
pub struct SongAudioSource {
    pub song: Song,
    pub cue: Option<(
        crate::models::music::CueTrack,
        crate::models::music::CueSource,
    )>,
}

fn invalid_cue(error: impl std::fmt::Display) -> MusicRepoError {
    MusicRepoError::new(MusicRepoErrorKind::InvalidInput, error.to_string())
}

/// Remove dependent tracks before their unreferenced sources, in the caller's transaction.
pub fn cleanup_cue_sources(conn: &mut SqliteConnection) -> Result<(), diesel::result::Error> {
    diesel::sql_query("DELETE FROM cue_tracks WHERE NOT EXISTS (SELECT 1 FROM songs WHERE songs.cue_track_id = cue_tracks.id)").execute(conn)?;
    diesel::sql_query("DELETE FROM cue_sources WHERE NOT EXISTS (SELECT 1 FROM cue_tracks WHERE cue_tracks.cue_source_id = cue_sources.id)").execute(conn)?;
    Ok(())
}

/// Repository for song database operations.
#[derive(Clone, Debug)]
pub struct SongRepository {
    pool: DbPool,
}

impl SongRepository {
    /// Stage source metadata inside the directory publication transaction.
    pub fn put_cue_source(
        conn: &mut SqliteConnection,
        sheet_path: &str,
        source: &crate::flac::CueSource,
    ) -> Result<(), MusicRepoError> {
        use crate::db::schema::cue_sources;
        let size = i64::try_from(source.fingerprint.size).map_err(invalid_cue)?;
        let seconds =
            i64::try_from(source.fingerprint.modified_ns / 1_000_000_000).map_err(invalid_cue)?;
        let nanos =
            i32::try_from(source.fingerprint.modified_ns % 1_000_000_000).map_err(invalid_cue)?;
        let values = (
            cue_sources::cue_content.eq(&source.cue_content),
            cue_sources::image_size.eq(size),
            cue_sources::image_modified_seconds.eq(seconds),
            cue_sources::image_modified_nanos.eq(nanos),
        );
        diesel::insert_into(cue_sources::table)
            .values((cue_sources::sheet_path.eq(sheet_path), values))
            .on_conflict(cue_sources::sheet_path)
            .do_update()
            .set(values)
            .execute(conn)?;
        Ok(())
    }

    /// Load typed metadata without a persisted frame map.
    pub fn cue_source(
        &self,
        sheet_path: &str,
    ) -> Result<Option<crate::models::music::CueSource>, MusicRepoError> {
        use crate::db::schema::cue_sources;
        let mut conn = self.pool.get()?;
        Ok(cue_sources::table
            .filter(cue_sources::sheet_path.eq(sheet_path))
            .select(crate::models::music::CueSource::as_select())
            .first(&mut conn)
            .optional()?)
    }

    /// Retain the relational identity while replacing its indexed boundaries.
    pub fn put_cue_track(
        conn: &mut SqliteConnection,
        cue: &crate::models::music::CueTrackSource,
    ) -> Result<i32, MusicRepoError> {
        use crate::db::schema::{cue_sources, cue_tracks};
        let source_id: i32 = cue_sources::table
            .filter(cue_sources::sheet_path.eq(&cue.sheet_path))
            .select(cue_sources::id)
            .first(conn)?;
        let number = i32::try_from(cue.number).map_err(invalid_cue)?;
        let values = (
            cue_tracks::start_frame.eq(i64::try_from(cue.frames.start).map_err(invalid_cue)?),
            cue_tracks::end_frame.eq(i64::try_from(cue.frames.end).map_err(invalid_cue)?),
            cue_tracks::byte_start.eq(i64::try_from(cue.bytes.start).map_err(invalid_cue)?),
            cue_tracks::byte_end.eq(i64::try_from(cue.bytes.end).map_err(invalid_cue)?),
        );
        diesel::insert_into(cue_tracks::table)
            .values((
                cue_tracks::cue_source_id.eq(source_id),
                cue_tracks::track_number.eq(number),
                values,
            ))
            .on_conflict((cue_tracks::cue_source_id, cue_tracks::track_number))
            .do_update()
            .set(values)
            .execute(conn)?;
        Ok(cue_tracks::table
            .filter(cue_tracks::cue_source_id.eq(source_id))
            .filter(cue_tracks::track_number.eq(number))
            .select(cue_tracks::id)
            .first(conn)?)
    }

    /// Song and source references are read from one SQLite snapshot.
    pub fn audio_source(&self, song_id: i32) -> Result<Option<SongAudioSource>, MusicRepoError> {
        use crate::db::schema::{cue_sources, cue_tracks};
        use crate::models::music::{CueSource, CueTrack};
        let mut conn = self.pool.get()?;
        conn.transaction(|conn| {
            let Some(row) = songs::table
                .find(song_id)
                .select(SongRow::as_select())
                .first::<SongRow>(conn)
                .optional()?
            else {
                return Ok(None);
            };
            let song = Song::from(row);
            let cue = song
                .cue_track_id
                .map(|id| {
                    cue_tracks::table
                        .find(id)
                        .inner_join(cue_sources::table)
                        .select((CueTrack::as_select(), CueSource::as_select()))
                        .first::<(CueTrack, CueSource)>(conn)
                })
                .transpose()?;
            Ok(Some(SongAudioSource { song, cue }))
        })
    }

    /// Validate staged replacements against their temporary native index.
    pub fn validate_cue_publication(
        conn: &mut SqliteConnection,
        sheet_path: &str,
        source: &crate::flac::CueSource,
    ) -> Result<(), MusicRepoError> {
        use crate::db::schema::{cue_sources, cue_tracks};
        use crate::models::music::CueTrack;
        #[derive(QueryableByName)]
        struct Conflict {
            #[diesel(sql_type = diesel::sql_types::BigInt)]
            count: i64,
        }
        let sheet =
            crate::scanner::cue::CueSheet::parse(&source.cue_content).map_err(invalid_cue)?;
        let sheet_path = std::path::Path::new(sheet_path);
        let image = std::path::Path::new(&source.image_path);
        if !sheet_path.is_absolute()
            || !image.is_absolute()
            || sheet_path.with_file_name(&sheet.image) != image
        {
            return Err(invalid_cue(
                "CUE image must be the sheet's same-directory source",
            ));
        }
        let expected = source
            .index
            .intervals(&sheet.tracks.iter().map(|t| t.ticks).collect::<Vec<_>>())
            .map_err(invalid_cue)?;
        let rows = songs::table
            .inner_join(cue_tracks::table.inner_join(cue_sources::table))
            .filter(cue_sources::sheet_path.eq(sheet_path.to_string_lossy().as_ref()))
            .select((SongRow::as_select(), CueTrack::as_select()))
            .order(cue_tracks::track_number.asc())
            .load::<(SongRow, CueTrack)>(conn)?;
        if rows.len() != expected.len() {
            return Err(invalid_cue("incomplete CUE song set"));
        }
        let mut owner = None;
        for ((song, track), (cue, frames)) in rows.iter().zip(sheet.tracks.iter().zip(expected)) {
            let folder_path: String = music_folders::table
                .find(song.music_folder_id)
                .select(music_folders::path)
                .first(conn)?;
            let selected = &source.index.frames[frames.clone()];
            let first = selected
                .first()
                .ok_or_else(|| invalid_cue("empty interval"))?;
            let last = selected
                .last()
                .ok_or_else(|| invalid_cue("empty interval"))?;
            let rate = i32::try_from(source.index.sample_rate).map_err(invalid_cue)?;
            if song.path != source.image_path
                || image.parent() != Some(std::path::Path::new(&folder_path))
                || owner.is_some_and(|id| id != song.music_folder_id)
                || rate <= 0
                || !(1..=8).contains(&source.index.channels)
                || !(4..=32).contains(&source.index.bit_depth)
                || song.sampling_rate != Some(rate)
                || song.channel_count != Some(i32::from(source.index.channels))
                || song.bit_depth != Some(i32::from(source.index.bit_depth))
                || song.track_number != Some(track.track_number)
                || u32::try_from(track.track_number).map_err(invalid_cue)? != cue.number
                || usize::try_from(track.start_frame).map_err(invalid_cue)? != frames.start
                || usize::try_from(track.end_frame).map_err(invalid_cue)? != frames.end
                || u64::try_from(track.byte_start).map_err(invalid_cue)? != first.offset
                || u64::try_from(track.byte_end).map_err(invalid_cue)? != last.offset + last.length
                || last.offset + last.length > source.fingerprint.size
            {
                return Err(invalid_cue("inconsistent CUE song metadata or boundaries"));
            }
            owner = Some(song.music_folder_id);
        }
        let conflicts = diesel::sql_query(
            "SELECT COUNT(*) AS count FROM (
            SELECT songs.path FROM songs JOIN cue_tracks ON songs.cue_track_id = cue_tracks.id
            WHERE songs.path = ?
            GROUP BY songs.path HAVING COUNT(DISTINCT cue_tracks.cue_source_id) > 1
        )",
        )
        .bind::<diesel::sql_types::Text, _>(&source.image_path)
        .get_result::<Conflict>(conn)?;
        if conflicts.count != 0 {
            return Err(invalid_cue("competing CUE image owners"));
        }
        Ok(())
    }

    /// Read only the songs directly owned by one directory.
    pub fn find_by_directory(&self, directory_id: i32) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;
        let rows = songs::table
            .filter(songs::music_folder_id.eq(directory_id))
            .select(SongRow::as_select())
            .load(&mut conn)?;
        Ok(rows.into_iter().map(Song::from).collect())
    }

    /// Create a new song repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Find a song by ID.
    pub fn find_by_id(&self, song_id: i32) -> Result<Option<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = songs::table
            .filter(songs::id.eq(song_id))
            .select(SongRow::as_select())
            .first(&mut conn)
            .optional()?;

        Ok(result.map(Song::from))
    }

    /// Find songs by album ID.
    pub fn find_by_album(&self, album_id: i32) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = songs::table
            .filter(songs::album_id.eq(album_id))
            .select(SongRow::as_select())
            .order((songs::disc_number.asc(), songs::track_number.asc()))
            .load(&mut conn)?;

        Ok(results.into_iter().map(Song::from).collect())
    }

    /// Find songs by artist ID.
    pub fn find_by_artist(&self, artist_id: i32) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = songs::table
            .filter(songs::artist_id.eq(artist_id))
            .select(SongRow::as_select())
            .order(songs::title.asc())
            .load(&mut conn)?;

        Ok(results.into_iter().map(Song::from).collect())
    }

    /// Find songs by music folder ID.
    pub fn find_by_music_folder(&self, folder_id: i32) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = songs::table
            .filter(songs::music_folder_id.eq_any(subtree_ids(&mut conn, folder_id)?))
            .select(SongRow::as_select())
            .order(songs::path.asc())
            .load(&mut conn)?;

        Ok(results.into_iter().map(Song::from).collect())
    }

    /// Search songs by title with pagination.
    /// An empty query returns all songs.
    pub fn search(
        &self,
        query: &str,
        music_folder_id: Option<i32>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let mut db_query = songs::table
            .select(SongRow::as_select())
            .order(songs::title.asc())
            .offset(offset)
            .limit(limit)
            .into_boxed();

        if !query.is_empty() {
            let pattern = search_pattern(query);
            db_query = db_query.filter(songs::search_name.like(pattern));
        }

        if let Some(folder_id) = music_folder_id {
            db_query =
                db_query.filter(songs::music_folder_id.eq_any(subtree_ids(&mut conn, folder_id)?));
        }

        let results = db_query.load(&mut conn)?;
        Ok(results.into_iter().map(Song::from).collect())
    }

    /// Get all genres with song and album counts.
    /// Returns a vector of (`genre_name`, `song_count`, `album_count`).
    pub fn get_genres(&self) -> Result<Vec<(String, i64, i64)>, MusicRepoError> {
        use std::collections::HashMap;

        let mut conn = self.pool.get()?;

        // Get song counts per genre
        let song_counts: Vec<(Option<String>, i64)> = songs::table
            .filter(songs::genre.is_not_null())
            .group_by(songs::genre)
            .select((songs::genre, diesel::dsl::count_star()))
            .load(&mut conn)?;

        // Get album counts per genre
        let album_counts: Vec<(Option<String>, i64)> = albums::table
            .filter(albums::genre.is_not_null())
            .group_by(albums::genre)
            .select((albums::genre, diesel::dsl::count_star()))
            .load(&mut conn)?;

        // Merge into a single list
        let mut genre_map: HashMap<String, (i64, i64)> = HashMap::new();

        for (genre, count) in song_counts {
            if let Some(g) = genre {
                genre_map.entry(g).or_insert((0, 0)).0 = count;
            }
        }

        for (genre, count) in album_counts {
            if let Some(g) = genre {
                genre_map.entry(g).or_insert((0, 0)).1 = count;
            }
        }

        let mut genres: Vec<(String, i64, i64)> = genre_map
            .into_iter()
            .map(|(name, (song_count, album_count))| (name, song_count, album_count))
            .collect();

        genres.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(genres)
    }

    /// Find random songs with optional filters.
    pub fn find_random(
        &self,
        size: i64,
        genre: Option<&str>,
        from_year: Option<i32>,
        to_year: Option<i32>,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let mut query = songs::table.into_boxed();

        if let Some(g) = genre {
            query = query.filter(songs::genre.eq(g));
        }

        if let Some(from) = from_year {
            query = query.filter(songs::year.ge(from));
        }

        if let Some(to) = to_year {
            query = query.filter(songs::year.le(to));
        }

        if let Some(folder_id) = music_folder_id {
            query = query.filter(songs::music_folder_id.eq_any(subtree_ids(&mut conn, folder_id)?));
        }

        let results = query
            .select(SongRow::as_select())
            .order(diesel::dsl::sql::<diesel::sql_types::Integer>("RANDOM()"))
            .limit(size)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Song::from).collect())
    }

    /// Find songs by genre with pagination.
    pub fn find_by_genre(
        &self,
        genre: &str,
        count: i64,
        offset: i64,
        music_folder_id: Option<i32>,
    ) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let mut query = songs::table.into_boxed();

        query = query.filter(songs::genre.eq(genre));

        if let Some(folder_id) = music_folder_id {
            query = query.filter(songs::music_folder_id.eq_any(subtree_ids(&mut conn, folder_id)?));
        }

        let results = query
            .select(SongRow::as_select())
            .order(songs::title.asc())
            .offset(offset)
            .limit(count)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Song::from).collect())
    }

    /// Find songs by IDs.
    pub fn find_by_ids(&self, song_ids: &[i32]) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = songs::table
            .filter(songs::id.eq_any(song_ids))
            .select(SongRow::as_select())
            .load(&mut conn)?;

        Ok(results.into_iter().map(Song::from).collect())
    }

    /// Find random songs by artist, excluding a specific song.
    /// Used for getSimilarSongs2 endpoint.
    pub fn find_random_by_artist(
        &self,
        artist_id: i32,
        exclude_song_id: i32,
        limit: i64,
    ) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = songs::table
            .filter(songs::artist_id.eq(artist_id))
            .filter(songs::id.ne(exclude_song_id))
            .select(SongRow::as_select())
            .order(diesel::dsl::sql::<diesel::sql_types::Integer>("RANDOM()"))
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Song::from).collect())
    }

    /// Find a song by artist name and title.
    pub fn find_by_artist_and_title(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Option<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let result = songs::table
            .filter(songs::artist_name.eq(artist))
            .filter(songs::title.eq(title))
            .select(SongRow::as_select())
            .first(&mut conn)
            .optional()?;

        Ok(result.map(Song::from))
    }

    /// Find top songs by artist name, ordered by play count.
    /// Used for getTopSongs endpoint.
    pub fn find_top_by_artist_name(
        &self,
        artist_name: &str,
        limit: i64,
    ) -> Result<Vec<Song>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let results = songs::table
            .filter(songs::artist_name.eq(artist_name))
            .select(SongRow::as_select())
            .order(songs::play_count.desc())
            .limit(limit)
            .load(&mut conn)?;

        Ok(results.into_iter().map(Song::from).collect())
    }
}

#[cfg(test)]
mod search_tests {
    use super::{AlbumRepository, ArtistRepository, SongRepository};
    use crate::db::schema::{albums, artists, music_folders, songs};
    use crate::db::{DbConfig, DbPool, initialize_database};
    use crate::models::music::normalize_search_text;
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

    /// Seed two folders, one artist+album per folder, one song each.
    /// Folder 1 gets accented names, folder 2 plain names.
    fn seed(pool: &DbPool) -> (i32, i32) {
        let mut conn = pool.get().expect("connection must checkout");

        for (id, name) in [(1, "lib1"), (2, "lib2")] {
            diesel::insert_into(music_folders::table)
                .values((
                    music_folders::id.eq(id),
                    music_folders::name.eq(name),
                    music_folders::path.eq(format!("/music/{name}")),
                ))
                .execute(&mut conn)
                .expect("folder must insert");
        }

        let artist_names = [(1, "Beyoncé"), (2, "Plain Artist")];
        for (id, name) in artist_names {
            diesel::insert_into(artists::table)
                .values((
                    artists::id.eq(id),
                    artists::name.eq(name),
                    artists::search_name.eq(normalize_search_text(name)),
                ))
                .execute(&mut conn)
                .expect("artist must insert");
        }

        let album_names = [(1, "Crème de la Crème", 1), (2, "Plain Album", 2)];
        for (id, name, artist_id) in album_names {
            diesel::insert_into(albums::table)
                .values((
                    albums::id.eq(id),
                    albums::name.eq(name),
                    albums::artist_id.eq(artist_id),
                    albums::search_name.eq(normalize_search_text(name)),
                ))
                .execute(&mut conn)
                .expect("album must insert");
        }

        let song_names = [(1, "Águas de Março", 1, 1, 1), (2, "Plain Song", 2, 2, 2)];
        for (id, title, album_id, artist_id, folder_id) in song_names {
            diesel::insert_into(songs::table)
                .values((
                    songs::id.eq(id),
                    songs::title.eq(title),
                    songs::search_name.eq(normalize_search_text(title)),
                    songs::album_id.eq(album_id),
                    songs::artist_id.eq(artist_id),
                    songs::music_folder_id.eq(folder_id),
                    songs::path.eq(format!("/music/lib{folder_id}/{id}.flac")),
                    songs::parent_path.eq(format!("/music/lib{folder_id}")),
                    songs::content_type.eq("audio/flac"),
                    songs::suffix.eq("flac"),
                ))
                .execute(&mut conn)
                .expect("song must insert");
        }

        (1, 2)
    }

    #[test]
    fn search_matches_folded_accents_and_case() {
        let pool = test_pool();
        seed(&pool);

        let artists = ArtistRepository::new(pool.clone())
            .search("beyonce", None, 0, 20)
            .expect("artist search must succeed");
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].name, "Beyoncé");

        let albums = AlbumRepository::new(pool.clone())
            .search("creme de la", None, 0, 20)
            .expect("album search must succeed");
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name, "Crème de la Crème");

        let songs = SongRepository::new(pool)
            .search("AGUAS DE MARCO", None, 0, 20)
            .expect("song search must succeed");
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].title, "Águas de Março");
    }

    #[test]
    fn search_filters_by_music_folder() {
        let pool = test_pool();
        let (folder1, folder2) = seed(&pool);

        let songs = SongRepository::new(pool.clone())
            .search("", Some(folder1), 0, 20)
            .expect("song search must succeed");
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].title, "Águas de Março");

        // Albums/artists are scoped through their songs' folder
        let albums = AlbumRepository::new(pool.clone())
            .search("", Some(folder2), 0, 20)
            .expect("album search must succeed");
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name, "Plain Album");

        let artists = ArtistRepository::new(pool.clone())
            .search("", Some(folder2), 0, 20)
            .expect("artist search must succeed");
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].name, "Plain Artist");

        // Query + folder filter compose
        let songs = SongRepository::new(pool)
            .search("aguas", Some(folder2), 0, 20)
            .expect("song search must succeed");
        assert!(songs.is_empty());
    }

    #[test]
    fn empty_query_returns_all() {
        let pool = test_pool();
        seed(&pool);

        let songs = SongRepository::new(pool)
            .search("", None, 0, 20)
            .expect("song search must succeed");
        assert_eq!(songs.len(), 2);
    }
}
