//! Key-value settings repository for server-level configuration.

use diesel::prelude::*;

use crate::db::connection::DbPool;
use crate::db::repo::error::MusicRepoError;
use crate::db::schema::settings;

/// Setting key for the Last.fm API key.
pub const SETTING_LASTFM_API_KEY: &str = "lastfm_api_key";
/// Setting key for the Last.fm API secret.
pub const SETTING_LASTFM_API_SECRET: &str = "lastfm_api_secret";
/// Setting key for the last library scan completion time (epoch millis).
pub const SETTING_LAST_SCAN_AT: &str = "last_scan_at";

/// Repository for key-value application settings.
#[derive(Clone, Debug)]
pub struct SettingsRepository {
    pool: DbPool,
}

impl SettingsRepository {
    /// Create a new settings repository.
    #[must_use]
    pub const fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Get a setting value by key, or `None` when unset.
    ///
    /// # Errors
    /// Returns an error if the database connection fails or the query fails.
    pub fn get(&self, key: &str) -> Result<Option<String>, MusicRepoError> {
        use crate::db::schema::settings::dsl;

        let mut conn = self.pool.get()?;
        dsl::settings
            .find(key)
            .select(dsl::value)
            .first::<String>(&mut conn)
            .optional()
            .map_err(MusicRepoError::from)
    }

    /// Set a setting value, replacing any existing value.
    ///
    /// # Errors
    /// Returns an error if the database connection fails or the query fails.
    pub fn set(&self, key: &str, value: &str) -> Result<(), MusicRepoError> {
        let mut conn = self.pool.get()?;
        diesel::replace_into(settings::table)
            .values((settings::key.eq(key), settings::value.eq(value)))
            .execute(&mut conn)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{SETTING_LASTFM_API_KEY, SettingsRepository};
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
    fn set_then_get_roundtrips() {
        let repo = SettingsRepository::new(test_pool());

        assert_eq!(repo.get(SETTING_LASTFM_API_KEY).unwrap(), None);

        repo.set(SETTING_LASTFM_API_KEY, "abc123").unwrap();
        assert_eq!(
            repo.get(SETTING_LASTFM_API_KEY).unwrap(),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn set_replaces_existing_value() {
        let repo = SettingsRepository::new(test_pool());

        repo.set(SETTING_LASTFM_API_KEY, "old").unwrap();
        repo.set(SETTING_LASTFM_API_KEY, "new").unwrap();

        assert_eq!(
            repo.get(SETTING_LASTFM_API_KEY).unwrap(),
            Some("new".to_string())
        );
    }
}
