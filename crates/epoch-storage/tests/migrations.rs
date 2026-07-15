use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use epoch_storage::{LATEST_SCHEMA_VERSION, Store};
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
