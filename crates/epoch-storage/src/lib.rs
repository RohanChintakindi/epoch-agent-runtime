//! Durable `SQLite` metadata storage for the Epoch runtime.

use std::{fmt::Write as _, path::Path, time::Duration};

use rusqlite::{Connection, ErrorCode, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Latest schema version understood by this build.
pub const LATEST_SCHEMA_VERSION: i64 = 5;

#[derive(Clone, Copy)]
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: [Migration; 5] = [
    Migration {
        version: 1,
        name: "initial_runtime_schema",
        sql: include_str!("0001_initial.sql"),
    },
    Migration {
        version: 2,
        name: "append_only_event_payloads",
        sql: include_str!("0002_append_only_events.sql"),
    },
    Migration {
        version: 3,
        name: "append_only_effect_history",
        sql: include_str!("0003_append_only_effect_history.sql"),
    },
    Migration {
        version: 4,
        name: "trusted_capability_authority",
        sql: include_str!("0004_trusted_capability_authority.sql"),
    },
    Migration {
        version: 5,
        name: "durable_branch_forks",
        sql: include_str!("0005_durable_branch_forks.sql"),
    },
];

const CREATE_MIGRATION_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS schema_migrations (
        version INTEGER PRIMARY KEY CHECK (version >= 1),
        name TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 255),
        checksum_sha256 TEXT NOT NULL CHECK (
            length(checksum_sha256) = 64 AND checksum_sha256 NOT GLOB '*[^0-9a-f]*'
        ),
        applied_at_unix_ms INTEGER NOT NULL CHECK (applied_at_unix_ms >= 0)
    ) STRICT;
";

/// A configured connection to Epoch's trusted metadata database.
#[derive(Debug)]
pub struct Store {
    connection: Connection,
}

impl Store {
    /// Opens a database, configures safety pragmas, and applies pending migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if the database cannot be opened, configured, or migrated.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let mut connection = Connection::open(path)?;
        configure_connection(&connection)?;
        apply_migrations(&mut connection, &MIGRATIONS)?;
        verify_foreign_keys(&connection)?;
        Ok(Self { connection })
    }

    #[must_use]
    pub const fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Returns mutable access for trusted components that need an explicit transaction.
    pub const fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// Returns the most recently applied migration version.
    ///
    /// # Errors
    ///
    /// Returns an error when migration metadata cannot be read.
    pub fn schema_version(&self) -> Result<i64, StorageError> {
        current_schema_version(&self.connection)
    }
}

fn apply_migrations(
    connection: &mut Connection,
    migrations: &[Migration],
) -> Result<(), StorageError> {
    validate_migration_plan(migrations)?;

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_database_identity(&transaction)?;
    transaction.execute_batch(CREATE_MIGRATION_TABLE)?;
    let applied_count = verify_migration_history(&transaction, migrations)?;

    for migration in &migrations[applied_count..] {
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations \
             (version, name, checksum_sha256, applied_at_unix_ms) \
             VALUES (?1, ?2, ?3, unixepoch('subsec') * 1000)",
            params![
                migration.version,
                migration.name,
                migration_checksum(migration)
            ],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

fn configure_connection(connection: &Connection) -> Result<(), StorageError> {
    connection.busy_timeout(Duration::from_secs(10))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "trusted_schema", false)?;

    let journal_mode: String = retry_sqlite_busy(|| {
        connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
    })?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StorageError::PragmaNotApplied {
            name: "journal_mode",
            expected: "wal".to_owned(),
            actual: journal_mode,
        });
    }

    connection.pragma_update(None, "synchronous", "FULL")?;
    let synchronous: i64 = connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
    if synchronous != 2 {
        return Err(StorageError::PragmaNotApplied {
            name: "synchronous",
            expected: "2".to_owned(),
            actual: synchronous.to_string(),
        });
    }

    let foreign_keys: i64 = connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
    if foreign_keys != 1 {
        return Err(StorageError::PragmaNotApplied {
            name: "foreign_keys",
            expected: "1".to_owned(),
            actual: foreign_keys.to_string(),
        });
    }
    Ok(())
}

fn retry_sqlite_busy<T>(mut operation: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    const DELAYS_MS: [u64; 9] = [1, 2, 4, 8, 16, 32, 64, 128, 256];

    for delay_ms in DELAYS_MS {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if is_sqlite_lock(&error) => {
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            Err(error) => return Err(error),
        }
    }
    operation()
}

fn is_sqlite_lock(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn validate_migration_plan(migrations: &[Migration]) -> Result<(), StorageError> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = i64::try_from(index + 1).map_err(|_| StorageError::InvalidMigrationPlan)?;
        if migration.version != expected || migration.name.is_empty() || migration.sql.is_empty() {
            return Err(StorageError::InvalidMigrationPlan);
        }
    }
    Ok(())
}

fn verify_database_identity(connection: &Connection) -> Result<(), StorageError> {
    if table_exists(connection, "schema_migrations")? {
        return Ok(());
    }

    let table_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_schema \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if table_count == 0 {
        Ok(())
    } else {
        Err(StorageError::MissingMigrationMetadata)
    }
}

fn verify_migration_history(
    connection: &Connection,
    migrations: &[Migration],
) -> Result<usize, StorageError> {
    let mut statement = connection
        .prepare("SELECT version, name, checksum_sha256 FROM schema_migrations ORDER BY version")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let applied = rows.collect::<Result<Vec<_>, _>>()?;

    for (index, (version, name, checksum)) in applied.iter().enumerate() {
        let Some(expected) = migrations.get(index) else {
            return Err(StorageError::UnsupportedSchema {
                found: *version,
                latest: LATEST_SCHEMA_VERSION,
            });
        };
        if *version > LATEST_SCHEMA_VERSION {
            return Err(StorageError::UnsupportedSchema {
                found: *version,
                latest: LATEST_SCHEMA_VERSION,
            });
        }
        if *version != expected.version {
            return Err(StorageError::MigrationGap {
                expected: expected.version,
                found: *version,
            });
        }
        if name != expected.name || checksum != &migration_checksum(expected) {
            return Err(StorageError::MigrationDrift { version: *version });
        }
    }

    Ok(applied.len())
}

fn migration_checksum(migration: &Migration) -> String {
    let digest = Sha256::digest(migration.sql.as_bytes());
    let mut checksum = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut checksum, "{byte:02x}").expect("writing to a String cannot fail");
    }
    checksum
}

fn current_schema_version(connection: &Connection) -> Result<i64, StorageError> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(StorageError::from)
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool, StorageError> {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(StorageError::from)
}

fn verify_foreign_keys(connection: &Connection) -> Result<(), StorageError> {
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    if statement.exists([])? {
        Err(StorageError::ForeignKeyCheckFailed)
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database schema version {found} is newer than supported version {latest}")]
    UnsupportedSchema { found: i64, latest: i64 },
    #[error("migration {version} does not match the migration compiled into this build")]
    MigrationDrift { version: i64 },
    #[error("migration history has a gap: expected version {expected}, found {found}")]
    MigrationGap { expected: i64, found: i64 },
    #[error("database contains tables but no Epoch migration metadata")]
    MissingMigrationMetadata,
    #[error("compiled migration plan must be nonempty and contiguous from version 1")]
    InvalidMigrationPlan,
    #[error("SQLite pragma {name} was not applied: expected {expected}, got {actual}")]
    PragmaNotApplied {
        name: &'static str,
        expected: String,
        actual: String,
    },
    #[error("SQLite foreign_key_check reported an integrity violation")]
    ForeignKeyCheckFailed,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

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

    #[test]
    fn transient_busy_errors_are_retried_during_initialization() {
        let attempts = Cell::new(0_u8);
        let result = retry_sqlite_busy(|| {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt < 2 {
                Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                    Some("database is locked".to_owned()),
                ))
            } else {
                Ok("wal")
            }
        })
        .expect("transient lock should clear");

        assert_eq!(result, "wal");
        assert_eq!(attempts.get(), 3);
    }

    #[test]
    fn non_lock_errors_are_not_retried() {
        let attempts = Cell::new(0_u8);
        let error = retry_sqlite_busy(|| -> rusqlite::Result<()> {
            attempts.set(attempts.get() + 1);
            Err(rusqlite::Error::InvalidQuery)
        })
        .expect_err("non-lock failure must be returned");

        assert!(matches!(error, rusqlite::Error::InvalidQuery));
        assert_eq!(attempts.get(), 1);
    }
}
