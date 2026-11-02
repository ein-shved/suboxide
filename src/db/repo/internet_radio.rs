//! Internet radio station repository operations.

use chrono::NaiveDateTime;
use diesel::prelude::*;

use crate::db::DbPool;
use crate::db::repo::error::MusicRepoError;
use crate::db::schema::internet_radio_stations;

/// An internet radio station.
#[derive(Debug, Clone)]
pub struct InternetRadioStation {
    pub id: i32,
    pub name: String,
    pub stream_url: String,
    pub home_page_url: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

/// Database row representation for internet radio stations.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = internet_radio_stations)]
#[diesel(check_for_backend(diesel::sqlite::Sqlite))]
pub struct InternetRadioStationRow {
    id: i32,
    name: String,
    stream_url: String,
    home_page_url: Option<String>,
    created_at: NaiveDateTime,
    updated_at: NaiveDateTime,
}

impl From<InternetRadioStationRow> for InternetRadioStation {
    fn from(row: InternetRadioStationRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            stream_url: row.stream_url,
            home_page_url: row.home_page_url,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Repository for internet radio station database operations.
#[derive(Clone, Debug)]
pub struct InternetRadioRepository {
    pool: DbPool,
}

impl InternetRadioRepository {
    /// Create a new internet radio repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Get all stations, sorted by name.
    pub fn find_all(&self) -> Result<Vec<InternetRadioStation>, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let rows: Vec<InternetRadioStationRow> = internet_radio_stations::table
            .select(InternetRadioStationRow::as_select())
            .order(internet_radio_stations::name.asc())
            .load(&mut conn)?;

        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Create a new station.
    pub fn create(
        &self,
        name: &str,
        stream_url: &str,
        home_page_url: Option<&str>,
    ) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;

        diesel::insert_into(internet_radio_stations::table)
            .values((
                internet_radio_stations::name.eq(name),
                internet_radio_stations::stream_url.eq(stream_url),
                internet_radio_stations::home_page_url.eq(home_page_url),
            ))
            .execute(&mut conn)?;

        Ok(())
    }

    /// Update an existing station. Returns whether the station existed.
    pub fn update(
        &self,
        id: i32,
        name: &str,
        stream_url: &str,
        home_page_url: Option<&str>,
    ) -> Result<bool, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let updated = diesel::update(internet_radio_stations::table.find(id))
            .set((
                internet_radio_stations::name.eq(name),
                internet_radio_stations::stream_url.eq(stream_url),
                internet_radio_stations::home_page_url.eq(home_page_url),
                internet_radio_stations::updated_at.eq(chrono::Utc::now().naive_utc()),
            ))
            .execute(&mut conn)?;

        Ok(updated > 0)
    }

    /// Delete a station. Returns whether the station existed.
    pub fn delete(&self, id: i32) -> Result<bool, MusicRepoError> {
        let mut conn = self.pool.get()?;

        let deleted = diesel::delete(internet_radio_stations::table.find(id)).execute(&mut conn)?;

        Ok(deleted > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::InternetRadioRepository;
    use crate::db::{DbConfig, DbPool, initialize_database};

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

    #[test]
    fn create_then_find_all_sorted_by_name() {
        let repo = InternetRadioRepository::new(test_pool());

        repo.create("Zulu FM", "https://z.example/stream", None)
            .expect("create must succeed");
        repo.create(
            "Alpha Radio",
            "https://a.example/stream",
            Some("https://a.example"),
        )
        .expect("create must succeed");

        let stations = repo.find_all().expect("stations must load");
        assert_eq!(stations.len(), 2);
        assert_eq!(stations[0].name, "Alpha Radio");
        assert_eq!(
            stations[0].home_page_url.as_deref(),
            Some("https://a.example")
        );
        assert_eq!(stations[1].name, "Zulu FM");
        assert_eq!(stations[1].home_page_url, None);
    }

    #[test]
    fn update_changes_fields_and_reports_existence() {
        let repo = InternetRadioRepository::new(test_pool());
        repo.create("Old", "https://old.example/stream", None)
            .expect("create must succeed");
        let id = repo.find_all().expect("load")[0].id;

        assert!(
            repo.update(
                id,
                "New",
                "https://new.example/stream",
                Some("https://new.example")
            )
            .expect("update must succeed")
        );
        assert!(
            !repo
                .update(id + 100, "New", "https://new.example/stream", None)
                .expect("update must succeed"),
            "missing station reports false"
        );

        let station = &repo.find_all().expect("load")[0];
        assert_eq!(station.name, "New");
        assert_eq!(station.stream_url, "https://new.example/stream");
    }

    #[test]
    fn delete_reports_existence() {
        let repo = InternetRadioRepository::new(test_pool());
        repo.create("FM", "https://fm.example/stream", None)
            .expect("create must succeed");
        let id = repo.find_all().expect("load")[0].id;

        assert!(repo.delete(id).expect("delete"));
        assert!(!repo.delete(id).expect("delete"));
        assert!(repo.find_all().expect("load").is_empty());
    }
}
