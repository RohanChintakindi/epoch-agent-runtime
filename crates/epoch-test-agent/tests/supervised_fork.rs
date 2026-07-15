#![cfg(unix)]

use std::{
    fs,
    sync::{Arc, Barrier},
};

use epoch_core::{BranchId, EventActor, EventKind, EventStatus, SessionId};
use epoch_events::{EventJournal, NewEvent};
use epoch_storage::Store;
use epoch_supervisor::{
    ApplicationCheckpointReport, ApplicationRestoreMode, BoundaryOutcome, DirectSupervisor,
    RecoveryCode, RecoveryOutcome,
};
use serde_json::json;
use tempfile::TempDir;

fn supported<T>(outcome: RecoveryOutcome<T>) -> T {
    match outcome {
        RecoveryOutcome::Supported(value) => value,
        RecoveryOutcome::Unsupported(issue) => panic!("unexpected unsupported: {issue:?}"),
        RecoveryOutcome::Failed(issue) => panic!("unexpected failure: {issue:?}"),
    }
}

struct Fixture {
    directory: TempDir,
    state_root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().expect("create fork fixture");
        let state_root = directory.path().join("state");
        Self {
            directory,
            state_root,
        }
    }

    fn checkpoint(&self, seed: u64) -> (SessionId, BranchId, ApplicationCheckpointReport) {
        let workspace = self.directory.path().join(format!("workspace-{seed}"));
        fs::create_dir(&workspace).expect("create declared fork workspace");
        let manifest = self.directory.path().join(format!("workload-{seed}.toml"));
        let executable = env!("CARGO_BIN_EXE_epoch-test-agent");
        fs::write(
            &manifest,
            format!(
                "schema_version = 1\n\
                 name = \"epoch-test-agent\"\n\
                 executable = \"{executable}\"\n\
                 working_directory = \"{}\"\n\
                 arguments = [\"--seed\", \"{seed}\", \"--scenario\", \"files\", \
                              \"--workspace\", \".\"]\n",
                workspace.display(),
            ),
        )
        .expect("write fork workload");
        let supervisor = DirectSupervisor::open(&self.state_root).expect("open supervisor");
        let run = supervisor.run_manifest(manifest).expect("run W02 workload");
        let checkpoint = supported(supervisor.checkpoint_application(
            run.session_id,
            Some(run.branch_id),
            Some("fork-source"),
        ));
        (run.session_id, run.branch_id, checkpoint)
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ParentSnapshot {
    branch: (String, i64, String),
    epoch: (String, String, i64),
    component: (String, String, String),
    event_count: i64,
    effect_count: i64,
}

fn parent_snapshot(state_root: &std::path::Path, branch_id: BranchId) -> ParentSnapshot {
    let store = Store::open(state_root.join("state.db")).expect("open parent state");
    let branch = store
        .connection()
        .query_row(
            "SELECT state, next_event_sequence, COALESCE(fork_epoch_id, '') \
             FROM branches WHERE id = ?1",
            [branch_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read parent branch");
    let epoch = store
        .connection()
        .query_row(
            "SELECT id, status, sequence FROM epochs WHERE branch_id = ?1",
            [branch_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read parent epoch");
    let component = store
        .connection()
        .query_row(
            "SELECT status, blob_hash, metadata_json FROM snapshot_components \
             WHERE epoch_id = ?1 AND kind = 'application_context'",
            [&epoch.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read parent component");
    let event_count = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM events WHERE branch_id = ?1",
            [branch_id.to_string()],
            |row| row.get(0),
        )
        .expect("count parent events");
    let effect_count = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM effect_intents WHERE branch_id = ?1",
            [branch_id.to_string()],
            |row| row.get(0),
        )
        .expect("count parent effects");
    ParentSnapshot {
        branch,
        epoch,
        component,
        event_count,
        effect_count,
    }
}

fn insert_effect(
    state_root: &std::path::Path,
    session_id: SessionId,
    branch_id: BranchId,
    checkpoint: &ApplicationCheckpointReport,
) {
    let store = Store::open(state_root.join("state.db")).expect("open effect fixture state");
    store
        .connection()
        .execute(
            "INSERT INTO effect_intents \
             (id, session_id, branch_id, operation_id, replay_key, action, resource, input_hash, \
              state, policy_revision, prepared_at_unix_ms) \
             VALUES ('fork-effect', ?1, ?2, 'fork-operation', 'fork-replay', 'write', 'fixture', \
                     ?3, 'succeeded', 0, 0)",
            [
                session_id.to_string(),
                branch_id.to_string(),
                checkpoint.component_hash.to_string(),
            ],
        )
        .expect("insert durable effect");
}

#[test]
fn fork_inherits_validated_context_without_mutating_parent_or_effects() {
    let fixture = Fixture::new();
    let (session_id, parent_branch_id, checkpoint) = fixture.checkpoint(801);
    insert_effect(
        &fixture.state_root,
        session_id,
        parent_branch_id,
        &checkpoint,
    );
    let before = parent_snapshot(&fixture.state_root, parent_branch_id);

    let supervisor = DirectSupervisor::open(&fixture.state_root).expect("restart supervisor");
    let fork = supported(supervisor.fork_application_epoch(checkpoint.epoch_id, "experiment"));
    assert_eq!(fork.session_id, session_id);
    assert_eq!(fork.parent_branch_id, parent_branch_id);
    assert_eq!(fork.fork_epoch_id, checkpoint.epoch_id);
    assert_eq!(fork.name, "experiment");
    assert_eq!(fork.fork_point_sequence, checkpoint.boundary_sequence);
    assert_eq!(fork.application_component_hash, checkpoint.component_hash);
    assert_eq!(fork.replay.recorded_results.model.len(), 1);
    assert_eq!(fork.replay.recorded_results.tool.len(), 2);
    assert_eq!(
        fork.replay.continuation.outcome,
        BoundaryOutcome::Unsupported
    );
    assert_eq!(fork.effect_frontier.outcome, BoundaryOutcome::Unsupported);
    assert_eq!(fork.effect_frontier.source_epoch_frontier, 0);

    assert_eq!(
        parent_snapshot(&fixture.state_root, parent_branch_id),
        before,
        "fork creation must not mutate parent state or non-rollbackable effects"
    );
    supported(supervisor.restore_application(
        checkpoint.epoch_id,
        ApplicationRestoreMode::Inspect,
        None,
    ));
    assert_eq!(
        parent_snapshot(&fixture.state_root, parent_branch_id),
        before
    );

    let restarted = DirectSupervisor::open(&fixture.state_root).expect("restart after fork");
    let inspected = supported(restarted.inspect_fork_branch(fork.branch_id));
    assert_eq!(inspected, fork);
    let inherited = supported(restarted.application_status(session_id, Some(fork.branch_id)));
    assert_eq!(inherited.current_epoch_id, Some(checkpoint.epoch_id));
    assert!(inherited.inherited_from_parent);
    assert_eq!(
        inherited
            .context
            .expect("inherited context")
            .cursors
            .boundary_sequence,
        checkpoint.boundary_sequence
    );
}

#[test]
fn child_event_sequence_and_state_are_independent_from_parent() {
    let fixture = Fixture::new();
    let (session_id, parent_branch_id, checkpoint) = fixture.checkpoint(802);
    let supervisor = DirectSupervisor::open(&fixture.state_root).expect("open supervisor");
    let fork = supported(supervisor.fork_application_epoch(checkpoint.epoch_id, "independent"));
    let journal = EventJournal::open(
        fixture.state_root.join("state.db"),
        fixture.state_root.join("blobs"),
    )
    .expect("open journal");
    let event = journal
        .append(NewEvent {
            session_id,
            branch_id: fork.branch_id,
            epoch_id: None,
            causal_parent: None,
            monotonic_ns: 0,
            occurred_at_unix_ms: 1,
            actor: EventActor::Supervisor,
            kind: EventKind::new("branch.replay_probe").expect("event kind"),
            input_hash: None,
            output_hash: None,
            status: EventStatus::Succeeded,
            payload: json!({"source_epoch_id": checkpoint.epoch_id}),
        })
        .expect("append child event");
    assert_eq!(event.sequence, 0);

    let store = Store::open(fixture.state_root.join("state.db")).expect("open state");
    store
        .connection()
        .execute(
            "UPDATE branches SET state = 'running', updated_at_unix_ms = updated_at_unix_ms + 1 \
             WHERE id = ?1",
            [fork.branch_id.to_string()],
        )
        .expect("advance child state");
    let parent_state: String = store
        .connection()
        .query_row(
            "SELECT state FROM branches WHERE id = ?1",
            [parent_branch_id.to_string()],
            |row| row.get(0),
        )
        .expect("read parent state");
    assert_eq!(parent_state, "completed");
}

#[test]
fn concurrent_same_name_forks_have_one_durable_winner() {
    let fixture = Fixture::new();
    let (session_id, _, checkpoint) = fixture.checkpoint(803);
    let state_root = Arc::new(fixture.state_root.clone());
    let barrier = Arc::new(Barrier::new(2));
    let handles = (0..2)
        .map(|_| {
            let state_root = Arc::clone(&state_root);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let supervisor = DirectSupervisor::open(state_root.as_path()).expect("open fork");
                barrier.wait();
                supervisor.fork_application_epoch(checkpoint.epoch_id, "collision")
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("fork thread"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RecoveryOutcome::Supported(_)))
            .count(),
        1
    );
    assert!(outcomes.iter().any(|outcome| matches!(
        outcome,
        RecoveryOutcome::Failed(issue) if issue.code == RecoveryCode::BranchNameConflict
    )));

    let status = DirectSupervisor::open(&fixture.state_root)
        .expect("restart supervisor")
        .session_status(session_id)
        .expect("read session");
    assert_eq!(status.branches.len(), 2);
}

#[test]
fn fork_rejects_invalid_name_uncommitted_epoch_and_corrupt_component() {
    let fixture = Fixture::new();
    let (_, _, checkpoint) = fixture.checkpoint(804);
    let supervisor = DirectSupervisor::open(&fixture.state_root).expect("open supervisor");
    for name in ["", "Has Spaces", "../escape", "UPPER"] {
        let RecoveryOutcome::Failed(issue) =
            supervisor.fork_application_epoch(checkpoint.epoch_id, name)
        else {
            panic!("invalid name must fail")
        };
        assert_eq!(issue.code, RecoveryCode::InvalidBranchName);
    }

    let store = Store::open(fixture.state_root.join("state.db")).expect("open state");
    store
        .connection()
        .execute(
            "UPDATE epochs SET status = 'creating', committed_at_unix_ms = NULL WHERE id = ?1",
            [checkpoint.epoch_id.to_string()],
        )
        .expect("make source epoch uncommitted");
    let RecoveryOutcome::Failed(issue) =
        supervisor.fork_application_epoch(checkpoint.epoch_id, "uncommitted")
    else {
        panic!("uncommitted epoch must fail")
    };
    assert_eq!(issue.code, RecoveryCode::MetadataMismatch);
    drop(store);

    let corrupt = Fixture::new();
    let (_, _, checkpoint) = corrupt.checkpoint(805);
    let blobs =
        epoch_blob::BlobStore::open(corrupt.state_root.join("blobs")).expect("open blob store");
    fs::write(blobs.blob_path(&checkpoint.component_hash), b"corrupt").expect("corrupt checkpoint");
    let supervisor = DirectSupervisor::open(&corrupt.state_root).expect("restart supervisor");
    let RecoveryOutcome::Failed(issue) =
        supervisor.fork_application_epoch(checkpoint.epoch_id, "corrupt")
    else {
        panic!("corrupt checkpoint must fail")
    };
    assert_eq!(issue.code, RecoveryCode::Integrity);

    let corrupt_workspace = Fixture::new();
    let (_, _, checkpoint) = corrupt_workspace.checkpoint(806);
    let blobs = epoch_blob::BlobStore::open(corrupt_workspace.state_root.join("blobs"))
        .expect("open workspace blob store");
    fs::write(
        blobs.blob_path(&checkpoint.workspace.manifest_hash),
        b"corrupt workspace manifest",
    )
    .expect("corrupt workspace manifest");
    let supervisor =
        DirectSupervisor::open(&corrupt_workspace.state_root).expect("restart supervisor");
    let RecoveryOutcome::Failed(issue) =
        supervisor.fork_application_epoch(checkpoint.epoch_id, "corrupt-workspace")
    else {
        panic!("corrupt composite workspace must fail fork")
    };
    assert_eq!(issue.code, RecoveryCode::Integrity);
}
