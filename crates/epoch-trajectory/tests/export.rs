use std::{collections::BTreeSet, path::Path};

use epoch_core::{BranchId, EpochId, EventId, SessionId};
use epoch_storage::Store;
use epoch_trajectory::{ExportLimits, PrivacyProfile, export_session};
use rusqlite::params;
use tempfile::TempDir;

const SECRET_PAYLOAD: &str = "secret-customer@example.com";
const TASK_GROUP: &str = "repo-17.issue-42";
const COMPONENT_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SECRET_SHAPED_KIND: &str = "customer_secret_token_abcdef";

struct Fixture {
    temp: TempDir,
    database: std::path::PathBuf,
    session: SessionId,
    root: BranchId,
    successful: BranchId,
}

#[allow(clippy::too_many_lines)]
fn fixture() -> Fixture {
    let temp = TempDir::new().expect("temporary fixture");
    let database = temp.path().join("state.db");
    let store = Store::open(&database).expect("open migrated store");
    let connection = store.connection();
    let session = SessionId::new();
    let root = BranchId::new();
    let fork_epoch = EpochId::new();
    let successful = BranchId::new();
    let failed = BranchId::new();
    let suspended = BranchId::new();

    connection
        .execute(
            "INSERT INTO sessions
             (id, state, policy_revision, revision, created_at_unix_ms, updated_at_unix_ms)
             VALUES (?1, 'completed', 4, 8, 1000, 9000)",
            [session.to_string()],
        )
        .expect("insert session");
    connection
        .execute(
            "INSERT INTO branches
             (id, session_id, state, next_event_sequence, created_at_unix_ms, updated_at_unix_ms)
             VALUES (?1, ?2, 'completed', 4, 1000, 2000)",
            params![root.to_string(), session.to_string()],
        )
        .expect("insert root branch");
    connection
        .execute(
            "INSERT INTO blobs (hash, byte_length, media_type, created_at_unix_ms)
             VALUES (?1, 1, 'application/json', 1500)",
            [COMPONENT_HASH],
        )
        .expect("insert component blob");
    connection
        .execute(
            "INSERT INTO epochs
             (id, session_id, branch_id, sequence, status, backend, policy_revision,
              effect_frontier, created_at_unix_ms, committed_at_unix_ms)
             VALUES (?1, ?2, ?3, 0, 'committed', 'application', 4, 0, 1500, 1600)",
            params![
                fork_epoch.to_string(),
                session.to_string(),
                root.to_string()
            ],
        )
        .expect("insert fork epoch");
    connection
        .execute(
            "INSERT INTO snapshot_components
             (epoch_id, kind, status, backend, blob_hash, checksum_sha256, byte_length,
              metadata_json, staged_at_unix_ms, committed_at_unix_ms)
             VALUES (?1, 'application_context', 'committed', 'application', ?2, ?2, 1,
                     '{\"boundary_sequence\":1}', 1500, 1600)",
            params![fork_epoch.to_string(), COMPONENT_HASH],
        )
        .expect("insert fork component");

    for (branch, name, state, created) in [
        (successful, "candidate-success", "promoted", 3000_i64),
        (failed, "candidate-failed", "failed", 4000_i64),
        (suspended, "candidate-pending", "suspended", 5000_i64),
    ] {
        connection
            .execute(
                "INSERT INTO branches
                 (id, session_id, parent_branch_id, fork_epoch_id, state, next_event_sequence,
                  created_at_unix_ms, updated_at_unix_ms, name, fork_point_sequence,
                  fork_component_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, 2, ?6, ?6 + 1000, ?7, 1, ?8)",
                params![
                    branch.to_string(),
                    session.to_string(),
                    root.to_string(),
                    fork_epoch.to_string(),
                    state,
                    created,
                    name,
                    COMPONENT_HASH
                ],
            )
            .expect("insert fork branch");
    }

    insert_event(connection, session, root, 0, "agent.start", "started", 10);
    insert_event(
        connection,
        session,
        root,
        1,
        SECRET_SHAPED_KIND,
        "succeeded",
        20,
    );
    insert_event(
        connection,
        session,
        root,
        2,
        "agent.completion",
        "succeeded",
        30,
    );
    insert_event(connection, session, root, 3, "tool.call", "started", 40);
    for (branch, status, offset) in [
        (successful, "succeeded", 100_u64),
        (failed, "failed", 200_u64),
        (suspended, "unknown", 300_u64),
    ] {
        insert_event(
            connection,
            session,
            branch,
            0,
            "tool.call",
            "started",
            offset,
        );
        insert_event(
            connection,
            session,
            branch,
            1,
            "tool.result",
            status,
            offset + 25,
        );
    }
    drop(store);

    Fixture {
        temp,
        database,
        session,
        root,
        successful,
    }
}

fn insert_event(
    connection: &rusqlite::Connection,
    session: SessionId,
    branch: BranchId,
    sequence: i64,
    kind: &str,
    status: &str,
    monotonic_ns: u64,
) {
    connection
        .execute(
            "INSERT INTO events
             (id, session_id, branch_id, sequence, monotonic_ns, occurred_at_unix_ms,
              actor, kind, status, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, 1700 + ?4, 'agent', ?6, ?7, ?8)",
            params![
                EventId::new().to_string(),
                session.to_string(),
                branch.to_string(),
                sequence,
                i64::try_from(monotonic_ns).expect("small monotonic time"),
                kind,
                status,
                serde_json::json!({
                    "private": SECRET_PAYLOAD,
                    "path": "/Users/example/private-repository",
                    "component_hash": COMPONENT_HASH,
                })
                .to_string(),
            ],
        )
        .expect("insert event");
}

#[test]
#[allow(clippy::too_many_lines)]
fn export_is_deterministic_grouped_labelled_and_payload_free() {
    let fixture = fixture();
    let first = export_session(
        &fixture.database,
        fixture.session,
        TASK_GROUP,
        ExportLimits::default(),
    )
    .expect("export trajectories");
    let second = export_session(
        &fixture.database,
        fixture.session,
        TASK_GROUP,
        ExportLimits::default(),
    )
    .expect("repeat export");
    assert_eq!(first, second);
    assert_eq!(first.len(), 4);
    assert!(first.iter().all(|record| {
        record.schema_version == 1 && record.privacy_profile == PrivacyProfile::MetadataOnly
    }));

    let success = first
        .iter()
        .find(|record| record.success_label == Some(true) && record.branch_depth == 1)
        .expect("successful branch");
    let failed = first
        .iter()
        .find(|record| record.success_label == Some(false))
        .expect("failed branch");
    let suspended = first
        .iter()
        .find(|record| record.success_label.is_none())
        .expect("suspended branch");
    let root = first
        .iter()
        .find(|record| record.success_label == Some(true) && record.branch_depth == 0)
        .expect("root branch");

    assert_eq!(success.success_label, Some(true));
    assert_eq!(success.value_label, Some(1.0));
    assert_eq!(failed.success_label, Some(false));
    assert_eq!(failed.value_label, Some(0.0));
    assert_eq!(suspended.success_label, None);
    assert_eq!(suspended.value_label, None);
    assert_eq!(root.value_label, Some(0.75));
    assert_eq!(root.branch_depth, 0);
    assert_eq!(success.branch_depth, 1);
    assert_eq!(success.candidate_group_id, failed.candidate_group_id);
    assert_eq!(failed.candidate_group_id, suspended.candidate_group_id);
    assert_eq!(success.events[1].delta_monotonic_ns, 25);
    assert_eq!(failed.summary.failed_events, 1);
    assert_eq!(suspended.summary.unknown_events, 1);
    assert_eq!(root.events.len(), 2);
    assert_eq!(root.events[0].kind, "agent.start");
    assert_eq!(root.events[1].kind, "other");

    let wire = serde_json::to_value(root).expect("trajectory wire JSON");
    let record_fields = wire
        .as_object()
        .expect("trajectory object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        record_fields,
        [
            "branch_depth",
            "candidate_group_id",
            "events",
            "privacy_profile",
            "schema_version",
            "session_group_id",
            "success_label",
            "summary",
            "task_group_id",
            "trajectory_id",
            "value_label",
        ]
        .into_iter()
        .collect()
    );
    let event_fields = wire["events"][0]
        .as_object()
        .expect("trajectory event object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        event_fields,
        [
            "actor",
            "delta_monotonic_ns",
            "has_causal_parent",
            "kind",
            "position",
            "references_epoch",
            "status",
        ]
        .into_iter()
        .collect()
    );
    let summary_fields = wire["summary"]
        .as_object()
        .expect("trajectory summary object")
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        summary_fields,
        [
            "denied_events",
            "duration_monotonic_ns",
            "event_count",
            "failed_events",
            "started_events",
            "succeeded_events",
            "unknown_events",
        ]
        .into_iter()
        .collect()
    );

    let encoded = first
        .iter()
        .map(|record| serde_json::to_string(record).expect("record JSON"))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        SECRET_PAYLOAD,
        TASK_GROUP,
        COMPONENT_HASH,
        "/Users/example/private-repository",
        &fixture.session.to_string(),
        &fixture.root.to_string(),
        &fixture.successful.to_string(),
        "branch_state",
        "agent.completion",
        "process.exited",
        "supervisor.failure",
        SECRET_SHAPED_KIND,
    ] {
        assert!(!encoded.contains(forbidden), "leaked {forbidden:?}");
    }
}

#[test]
fn export_rejects_invalid_groups_missing_sessions_and_limits_without_partial_results() {
    let fixture = fixture();
    for task_group in ["", "contains spaces", "UPPERCASE", &"x".repeat(129)] {
        let error = export_session(
            &fixture.database,
            fixture.session,
            task_group,
            ExportLimits::default(),
        )
        .expect_err("invalid task group");
        assert!(error.to_string().contains("task group"));
    }

    let missing = export_session(
        &fixture.database,
        SessionId::new(),
        TASK_GROUP,
        ExportLimits::default(),
    )
    .expect_err("unknown session");
    assert!(missing.to_string().contains("session"));

    let bounded = export_session(
        &fixture.database,
        fixture.session,
        TASK_GROUP,
        ExportLimits {
            max_branches: 3,
            max_events_per_branch: 10,
        },
    )
    .expect_err("branch limit");
    assert!(bounded.to_string().contains("branch limit"));
}

#[test]
fn jsonl_writer_refuses_existing_or_non_private_output() {
    let fixture = fixture();
    let records = export_session(
        &fixture.database,
        fixture.session,
        TASK_GROUP,
        ExportLimits::default(),
    )
    .expect("export trajectories");
    let output = fixture.temp.path().join("trajectories.jsonl");
    epoch_trajectory::write_jsonl_new(&output, &records).expect("write new JSONL");
    let metadata = std::fs::metadata(&output).expect("output metadata");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
    let lines = std::fs::read_to_string(&output)
        .expect("read JSONL")
        .lines()
        .count();
    assert_eq!(lines, records.len());
    let error = epoch_trajectory::write_jsonl_new(&output, &records)
        .expect_err("existing output must not be overwritten");
    assert!(error.to_string().contains("already exists"));

    let directory = fixture.temp.path().join("directory-output");
    std::fs::create_dir(&directory).expect("output directory");
    let error = epoch_trajectory::write_jsonl_new(Path::new(&directory), &records)
        .expect_err("directory is not a valid output");
    assert!(error.to_string().contains("output"));
}
