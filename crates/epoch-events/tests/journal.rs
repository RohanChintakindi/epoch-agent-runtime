use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use epoch_blob::{BlobHash, BlobStore};
use epoch_core::{BranchId, EpochId, EventActor, EventId, EventKind, EventStatus, SessionId};
use epoch_events::{EventJournal, EventQuery, INLINE_PAYLOAD_LIMIT, JournalError, NewEvent};
use epoch_storage::Store;
use rusqlite::params;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    database: PathBuf,
    blobs: PathBuf,
    session: SessionId,
    other_session: SessionId,
    branch: BranchId,
    sibling: BranchId,
    other_branch: BranchId,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().expect("create test runtime");
        let database = directory.path().join("state.db");
        let blobs = directory.path().join("blobs");
        let session = SessionId::new();
        let other_session = SessionId::new();
        let branch = BranchId::new();
        let sibling = BranchId::new();
        let other_branch = BranchId::new();

        let store = Store::open(&database).expect("open fixture database");
        for session_id in [session, other_session] {
            store
                .connection()
                .execute(
                    "INSERT INTO sessions (id, state, created_at_unix_ms, updated_at_unix_ms) \
                     VALUES (?1, 'running', 0, 0)",
                    [session_id.to_string()],
                )
                .expect("insert fixture session");
        }
        for (branch_id, session_id) in [
            (branch, session),
            (sibling, session),
            (other_branch, other_session),
        ] {
            store
                .connection()
                .execute(
                    "INSERT INTO branches \
                     (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
                     VALUES (?1, ?2, 'running', 0, 0)",
                    params![branch_id.to_string(), session_id.to_string()],
                )
                .expect("insert fixture branch");
        }
        drop(store);

        Self {
            _directory: directory,
            database,
            blobs,
            session,
            other_session,
            branch,
            sibling,
            other_branch,
        }
    }

    fn journal(&self) -> EventJournal {
        EventJournal::open(&self.database, &self.blobs).expect("open event journal")
    }
}

fn draft(session_id: SessionId, branch_id: BranchId, kind: &str, payload: Value) -> NewEvent {
    NewEvent {
        session_id,
        branch_id,
        epoch_id: None,
        causal_parent: None,
        monotonic_ns: 100,
        occurred_at_unix_ms: 1_750_000_000_000,
        actor: EventActor::Supervisor,
        kind: EventKind::new(kind).expect("valid fixture kind"),
        input_hash: None,
        output_hash: None,
        status: EventStatus::Succeeded,
        payload,
    }
}

#[test]
fn append_allocates_branch_sequences_and_queries_with_deterministic_filters() {
    let fixture = Fixture::new();
    let journal = fixture.journal();

    let first = journal
        .append(draft(
            fixture.session,
            fixture.branch,
            "tool.call",
            json!({"n": 1}),
        ))
        .expect("append first event");
    let sibling = journal
        .append(draft(
            fixture.session,
            fixture.sibling,
            "process.exec",
            json!({"command": "true"}),
        ))
        .expect("append sibling event");
    let second = journal
        .append(draft(
            fixture.session,
            fixture.branch,
            "tool.call",
            json!({"n": 2}),
        ))
        .expect("append second event");

    assert_eq!(
        (first.sequence, second.sequence, sibling.sequence),
        (0, 1, 0)
    );

    let all = journal
        .query(&EventQuery::for_session(fixture.session))
        .expect("query session events");
    let actual = all
        .iter()
        .map(|event| {
            (
                event.branch_id.to_string(),
                event.sequence,
                event.event_id.to_string(),
            )
        })
        .collect::<Vec<_>>();
    let mut expected = actual.clone();
    expected.sort();
    assert_eq!(actual, expected, "query order must be deterministic");
    assert!(all.contains(&first));
    assert!(all.contains(&second));
    assert!(all.contains(&sibling));
    assert_eq!(
        journal.read_payload(&first).expect("read inline payload"),
        json!({"n": 1})
    );

    let filtered = journal
        .query(&EventQuery {
            session_id: fixture.session,
            branch_id: Some(fixture.branch),
            kind: Some(EventKind::new("tool.call").expect("valid kind")),
            sequence: Some(1..=1),
        })
        .expect("query filtered events");
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].event_id, second.event_id);
}

#[test]
fn concurrent_appends_across_connections_never_duplicate_a_branch_sequence() {
    const WRITERS: usize = 12;

    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(WRITERS));
    let handles = (0..WRITERS)
        .map(|writer| {
            let database = fixture.database.clone();
            let blobs = fixture.blobs.clone();
            let barrier = Arc::clone(&barrier);
            let session = fixture.session;
            let branch = fixture.branch;
            thread::spawn(move || {
                let journal = EventJournal::open(database, blobs).expect("open writer journal");
                barrier.wait();
                journal.append(draft(
                    session,
                    branch,
                    "writer.tick",
                    json!({"writer": writer}),
                ))
            })
        })
        .collect::<Vec<_>>();

    let mut sequences = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("writer did not panic")
                .expect("append succeeded")
                .sequence
        })
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    assert_eq!(sequences, (0..WRITERS as u64).collect::<Vec<_>>());
}

#[test]
fn causal_parents_and_epoch_references_cannot_cross_scope() {
    let fixture = Fixture::new();
    let journal = fixture.journal();
    let parent = journal
        .append(draft(fixture.session, fixture.branch, "parent", json!({})))
        .expect("append parent");

    let mut cross_branch = draft(fixture.session, fixture.sibling, "child", json!({}));
    cross_branch.causal_parent = Some(parent.event_id);
    assert!(matches!(
        journal.append(cross_branch),
        Err(JournalError::CausalParentScopeMismatch { event_id }) if event_id == parent.event_id
    ));

    let missing_parent = EventId::new();
    let mut missing = draft(fixture.session, fixture.branch, "child", json!({}));
    missing.causal_parent = Some(missing_parent);
    assert!(matches!(
        journal.append(missing),
        Err(JournalError::CausalParentNotFound { event_id }) if event_id == missing_parent
    ));

    let epoch = EpochId::new();
    let store = Store::open(&fixture.database).expect("open fixture database");
    store
        .connection()
        .execute(
            "INSERT INTO epochs \
             (id, session_id, branch_id, sequence, status, created_at_unix_ms, committed_at_unix_ms) \
             VALUES (?1, ?2, ?3, 0, 'committed', 0, 0)",
            params![
                epoch.to_string(),
                fixture.session.to_string(),
                fixture.sibling.to_string()
            ],
        )
        .expect("insert sibling epoch");
    drop(store);
    let mut cross_epoch = draft(fixture.session, fixture.branch, "checkpoint", json!({}));
    cross_epoch.epoch_id = Some(epoch);
    assert!(matches!(
        journal.append(cross_epoch),
        Err(JournalError::EpochScopeMismatch { epoch_id }) if epoch_id == epoch
    ));

    let mut valid_child = draft(fixture.session, fixture.branch, "child", json!({}));
    valid_child.causal_parent = Some(parent.event_id);
    let child = journal.append(valid_child).expect("append valid child");
    assert_eq!(
        child.sequence, 1,
        "rejected appends must not consume sequence numbers"
    );
}

#[test]
fn branch_and_query_session_scope_mismatches_are_typed_errors() {
    let fixture = Fixture::new();
    let journal = fixture.journal();

    let error = journal
        .append(draft(
            fixture.session,
            fixture.other_branch,
            "wrong.scope",
            json!({}),
        ))
        .expect_err("cross-session append must fail");
    assert!(matches!(
        error,
        JournalError::BranchSessionMismatch {
            branch_id,
            expected,
            actual,
        } if branch_id == fixture.other_branch
            && expected == fixture.session
            && actual == fixture.other_session
    ));

    let error = journal
        .query(&EventQuery {
            session_id: fixture.session,
            branch_id: Some(fixture.other_branch),
            kind: None,
            sequence: None,
        })
        .expect_err("cross-session query must fail");
    assert!(matches!(error, JournalError::BranchSessionMismatch { .. }));
}

#[test]
fn large_payload_is_replaced_by_a_verified_blob_reference() {
    let fixture = Fixture::new();
    let journal = fixture.journal();
    let payload = json!({"data": "x".repeat(INLINE_PAYLOAD_LIMIT + 1)});

    let event = journal
        .append(draft(
            fixture.session,
            fixture.branch,
            "model.response",
            payload.clone(),
        ))
        .expect("append large payload");

    assert_eq!(event.payload_json, "{}");
    let hash: BlobHash = event
        .payload_blob_hash
        .as_deref()
        .expect("large payload has blob hash")
        .parse()
        .expect("valid blob hash");
    let store = Store::open(&fixture.database).expect("open database");
    let recorded: (i64, String) = store
        .connection()
        .query_row(
            "SELECT byte_length, media_type FROM blobs WHERE hash = ?1",
            [hash.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("blob metadata exists");
    let inline_limit = i64::try_from(INLINE_PAYLOAD_LIMIT).expect("inline limit fits SQLite");
    assert!(recorded.0 > inline_limit);
    assert_eq!(recorded.1, "application/json");
    let blobs = BlobStore::open(&fixture.blobs).expect("open blobs");
    assert!(blobs.blob_path(&hash).is_file());
    assert_eq!(
        journal.read_payload(&event).expect("read large payload"),
        payload
    );
}

#[test]
fn missing_input_or_output_blob_is_rejected_without_consuming_sequence() {
    let fixture = Fixture::new();
    let journal = fixture.journal();
    let missing = BlobHash::digest(b"not stored");
    let mut invalid = draft(fixture.session, fixture.branch, "tool.result", json!({}));
    invalid.output_hash = Some(missing.clone());
    assert!(matches!(
        journal.append(invalid),
        Err(JournalError::ReferencedBlobNotFound { role: "output", hash }) if hash == missing
    ));

    let event = journal
        .append(draft(fixture.session, fixture.branch, "next", json!({})))
        .expect("append after rejection");
    assert_eq!(event.sequence, 0);
}

#[test]
fn database_triggers_reject_event_update_and_delete() {
    let fixture = Fixture::new();
    let journal = fixture.journal();
    let event = journal
        .append(draft(
            fixture.session,
            fixture.branch,
            "immutable",
            json!({}),
        ))
        .expect("append event");
    drop(journal);

    let store = Store::open(&fixture.database).expect("open database");
    for statement in [
        "UPDATE events SET status = 'failed' WHERE id = ?1",
        "DELETE FROM events WHERE id = ?1",
    ] {
        store
            .connection()
            .execute(statement, [event.event_id.to_string()])
            .expect_err("append-only trigger must reject mutation");
    }
    let count: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE id = ?1 AND status = 'succeeded'",
            [event.event_id.to_string()],
            |row| row.get(0),
        )
        .expect("read immutable event");
    assert_eq!(count, 1);
}

#[test]
fn invalid_sequence_range_is_rejected() {
    let fixture = Fixture::new();
    let error = fixture
        .journal()
        .query(&EventQuery {
            session_id: fixture.session,
            branch_id: None,
            kind: None,
            sequence: Some(std::ops::RangeInclusive::new(2, 1)),
        })
        .expect_err("descending sequence range must fail");
    assert!(matches!(
        error,
        JournalError::InvalidSequenceRange { start: 2, end: 1 }
    ));
}
