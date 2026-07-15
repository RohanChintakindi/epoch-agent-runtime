//! Durable SQLite metadata storage for the Epoch runtime.

use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;

/// Latest schema version understood by this build.
pub const LATEST_SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Copy)]
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

/// A configured connection to Epoch's trusted metadata database.
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens a database, configures safety pragmas, and applies pending migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened, configured, or migrated.
    pub fn open(_path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Err(StorageError::NotImplemented)
    }

    #[must_use]
    pub const fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Returns the most recently applied migration version.
    ///
    /// # Errors
    ///
    /// Returns an error when migration metadata cannot be read.
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        Ok(0)
    }
}

fn apply_migrations(
    _connection: &mut Connection,
    _migrations: &[Migration],
) -> Result<(), StorageError> {
    Err(StorageError::NotImplemented)
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("SQLite storage is not implemented")]
    NotImplemented,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_MIGRATION: Migration = Migration {
        version: 1,
        name: "base",
        sql: "CREATE TABLE stable (id INTEGER PRIMARY KEY);",
    };
    const BROKEN_MIGRATION: Migration = Migration {
        version: 2,
        name: "broken",
        sql: "CREATE TABLE leaked (id INTEGER PRIMARY KEY); THIS IS NOT SQL;",
    };

    fn table_exists(connection: &Connection, name: &str) -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
                [name],
                |row| row.get(0),
            )
            .expect("query table existence")
    }

    #[test]
    fn failed_migration_rolls_back_without_damaging_previous_schema() {
        let mut connection = Connection::open_in_memory().expect("open database");
        apply_migrations(&mut connection, &[BASE_MIGRATION]).expect("apply base migration");

        let error = apply_migrations(&mut connection, &[BASE_MIGRATION, BROKEN_MIGRATION])
            .expect_err("broken migration must fail");
        assert!(matches!(error, StorageError::Sqlite(_)));
        assert!(table_exists(&connection, "stable"));
        assert!(!table_exists(&connection, "leaked"));
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("read schema version");
        assert_eq!(version, 1);
    }
}
