use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use epoch_storage::{LATEST_SCHEMA_VERSION, StorageError, Store};
use rusqlite::{ErrorCode, params};

const EXPECTED_TABLES: [&str; 15] = [
    "approvals",
    "benchmark_runs",
    "blobs",
    "branches",
    "capabilities",
    "effect_attempts",
    "effect_intents",
    "epochs",
    "events",
    "fault_injections",
    "schema_migrations",
    "semantic_diffs",
    "semantic_manifests",
    "sessions",
    "snapshot_components",
];

struct TestDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TestDatabase {
    fn new(name: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "epoch-storage-{name}-{}-{}",
            std::process::id(),
            uuid_suffix()
        ));
        fs::create_dir_all(&directory).expect("create test database directory");
        let path = directory.join("state.db");
        Self { directory, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).ok();
    }
}

fn uuid_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos()
        .to_string()
}

fn table_names(store: &Store) -> BTreeSet<String> {
    let mut statement = store
        .connection()
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .expect("prepare schema query");
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query schema")
        .collect::<Result<_, _>>()
        .expect("collect table names")
}

#[test]
fn fresh_database_migrates_to_the_complete_latest_schema() {
    let database = TestDatabase::new("fresh");
    let store = Store::open(database.path()).expect("open fresh database");

    assert_eq!(
        store.schema_version().expect("schema version"),
        LATEST_SCHEMA_VERSION
    );
    assert_eq!(
        table_names(&store),
        EXPECTED_TABLES.map(str::to_owned).into_iter().collect()
    );
}

#[test]
fn reopening_an_existing_database_is_idempotent() {
    let database = TestDatabase::new("reopen");
    {
        let store = Store::open(database.path()).expect("first open");
        assert_eq!(
            store.schema_version().expect("schema version"),
            LATEST_SCHEMA_VERSION
        );
    }

    let reopened = Store::open(database.path()).expect("second open");
    let migration_count: i64 = reopened
        .connection()
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("count migrations");
    assert_eq!(migration_count, LATEST_SCHEMA_VERSION);
    assert_eq!(
        reopened.schema_version().expect("schema version"),
        LATEST_SCHEMA_VERSION
    );
}

#[test]
fn foreign_keys_are_enabled_and_reject_orphaned_rows() {
    let database = TestDatabase::new("foreign-keys");
    let store = Store::open(database.path()).expect("open database");
    let enabled: i64 = store
        .connection()
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .expect("read foreign key pragma");
    assert_eq!(enabled, 1);

    let error = store
        .connection()
        .execute(
            "INSERT INTO branches (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
             VALUES (?1, ?2, 'created', 0, 0)",
            params!["00000000-0000-0000-0000-000000000001", "missing-session"],
        )
        .expect_err("orphan branch must be rejected");
    assert_eq!(
        error.sqlite_error_code(),
        Some(ErrorCode::ConstraintViolation)
    );
}

#[test]
fn database_uses_wal_and_full_synchronous_mode() {
    let database = TestDatabase::new("pragmas");
    let store = Store::open(database.path()).expect("open database");
    let journal_mode: String = store
        .connection()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal mode");
    let synchronous: i64 = store
        .connection()
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .expect("read synchronous mode");

    assert_eq!(journal_mode, "wal");
    assert_eq!(synchronous, 2, "SQLite FULL synchronous mode is integer 2");
}

#[test]
fn reopening_rejects_a_database_from_a_newer_binary() {
    let database = TestDatabase::new("future-schema");
    {
        let store = Store::open(database.path()).expect("open database");
        store
            .connection()
            .execute(
                "INSERT INTO schema_migrations (version, name, checksum_sha256, applied_at_unix_ms) \
                 VALUES (?1, 'future', ?2, 0)",
                params![LATEST_SCHEMA_VERSION + 1, "f".repeat(64)],
            )
            .expect("insert future migration marker");
    }

    let error = Store::open(database.path()).expect_err("future schema must be rejected");
    assert!(matches!(
        error,
        StorageError::UnsupportedSchema {
            found,
            latest: LATEST_SCHEMA_VERSION
        } if found == LATEST_SCHEMA_VERSION + 1
    ));
}

#[test]
fn reopening_detects_modified_migration_history() {
    let database = TestDatabase::new("migration-drift");
    {
        let store = Store::open(database.path()).expect("open database");
        store
            .connection()
            .execute(
                "UPDATE schema_migrations SET checksum_sha256 = ?1 WHERE version = 1",
                ["f".repeat(64)],
            )
            .expect("tamper with migration metadata");
    }

    let error = Store::open(database.path()).expect_err("migration drift must be rejected");
    assert!(matches!(error, StorageError::MigrationDrift { version: 1 }));
}

#[test]
fn composite_foreign_keys_reject_cross_session_epochs() {
    let database = TestDatabase::new("cross-session");
    let store = Store::open(database.path()).expect("open database");
    for session_id in ["session-a", "session-b"] {
        store
            .connection()
            .execute(
                "INSERT INTO sessions (id, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, 'created', 0, 0)",
                [session_id],
            )
            .expect("insert session");
    }
    store
        .connection()
        .execute(
            "INSERT INTO branches (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
             VALUES ('branch-a', 'session-a', 'created', 0, 0)",
            [],
        )
        .expect("insert branch");

    let error = store
        .connection()
        .execute(
            "INSERT INTO epochs \
             (id, session_id, branch_id, sequence, status, created_at_unix_ms) \
             VALUES ('epoch-a', 'session-b', 'branch-a', 0, 'creating', 0)",
            [],
        )
        .expect_err("epoch cannot claim another session's branch");
    assert_eq!(
        error.sqlite_error_code(),
        Some(ErrorCode::ConstraintViolation)
    );
}

#[test]
fn concurrent_first_open_is_serialized_and_idempotent() {
    use std::sync::{Arc, Barrier};

    let database = TestDatabase::new("concurrent-open");
    let path = Arc::new(database.path().to_owned());
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                Store::open(path.as_path()).map(|store| store.schema_version())
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        assert_eq!(
            handle
                .join()
                .expect("open thread must not panic")
                .expect("concurrent open must succeed")
                .expect("read schema version"),
            LATEST_SCHEMA_VERSION
        );
    }
}
