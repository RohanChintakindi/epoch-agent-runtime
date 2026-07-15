#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _};

use epoch_blob::BlobStore;
use epoch_checkpoint::{APPLICATION_CONTEXT_MEDIA_TYPE, ApplicationContext};
use epoch_core::{BranchId, SessionId};
use epoch_storage::Store;
use epoch_supervisor::{
    ApplicationCheckpointReport, ApplicationRestoreMode, DirectSupervisor, RecoveryCode,
    RecoveryOutcome, RestoreScope,
};
use tempfile::TempDir;

fn supported<T>(outcome: RecoveryOutcome<T>) -> T {
    match outcome {
        RecoveryOutcome::Supported(value) => value,
        RecoveryOutcome::Unsupported(issue) => panic!("unexpected unsupported outcome: {issue:?}"),
        RecoveryOutcome::Failed(issue) => panic!("unexpected failed outcome: {issue:?}"),
    }
}

fn run_and_checkpoint(
    fixture: &TempDir,
    seed: u64,
) -> (
    std::path::PathBuf,
    std::path::PathBuf,
    SessionId,
    BranchId,
    ApplicationCheckpointReport,
) {
    let state_root = fixture.path().join("state");
    let workspace = fixture.path().join(format!("workspace-{seed}"));
    let manifest = fixture.path().join(format!("workload-{seed}.toml"));
    let executable = env!("CARGO_BIN_EXE_epoch-test-agent");
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\n\
             name = \"epoch-test-agent\"\n\
             executable = \"{executable}\"\n\
             arguments = [\"--seed\", \"{seed}\", \"--scenario\", \"files\", \
                          \"--workspace\", \"{}\"]\n",
            workspace.display()
        ),
    )
    .expect("write W02 manifest");
    let supervisor = DirectSupervisor::open(&state_root).expect("open supervisor");
    let run = supervisor
        .run_manifest(&manifest)
        .expect("run deterministic W02 agent");
    let checkpoint = supported(supervisor.checkpoint_application(
        run.session_id,
        Some(run.branch_id),
        Some("before-mutation"),
    ));
    (
        state_root,
        workspace,
        run.session_id,
        run.branch_id,
        checkpoint,
    )
}

#[test]
fn three_restart_safe_run_checkpoint_mutate_restore_inspect_cycles_need_no_repair() {
    let fixture = TempDir::new().expect("create recovery fixture");

    for seed in [11_u64, 22, 33] {
        let (state_root, workspace, session_id, branch_id, checkpoint) =
            run_and_checkpoint(&fixture, seed);
        assert_eq!(
            checkpoint.restore_scope,
            RestoreScope::ApplicationContextOnly
        );
        assert_eq!(checkpoint.context_revision, 1);
        assert_eq!(checkpoint.boundary_sequence, 9);

        let artifact = workspace.join("artifact.txt");
        fs::write(&artifact, format!("advanced-after-{seed}"))
            .expect("mutate workspace after checkpoint");

        let restarted = DirectSupervisor::open(&state_root).expect("restart supervisor");
        let restored = supported(
            restarted.restore_application(checkpoint.epoch_id, ApplicationRestoreMode::Activate),
        );
        assert!(restored.activated);
        assert!(!restored.process_restored);
        assert!(!restored.workspace_restored);
        assert_eq!(restored.context.safe_point_id, checkpoint.safe_point_id);
        assert_eq!(restored.context.cursors.boundary_sequence, 9);

        let status = supported(restarted.application_status(session_id, Some(branch_id)));
        assert_eq!(status.current_epoch_id, Some(checkpoint.epoch_id));
        assert_eq!(
            status
                .context
                .expect("activated application context")
                .cursors
                .boundary_sequence,
            9
        );
        assert_eq!(
            fs::read_to_string(artifact).expect("read post-restore workspace"),
            format!("advanced-after-{seed}"),
            "application restore must not falsely claim workspace rollback"
        );
    }
}

#[test]
fn restore_rejects_corrupt_and_missing_component_bytes() {
    for (seed, remove, expected) in [
        (41_u64, false, RecoveryCode::Integrity),
        (42_u64, true, RecoveryCode::Storage),
    ] {
        let fixture = TempDir::new().expect("create rejection fixture");
        let (state_root, _, _, _, checkpoint) = run_and_checkpoint(&fixture, seed);
        let blobs = BlobStore::open(state_root.join("blobs")).expect("open blob store");
        let component = blobs.blob_path(&checkpoint.component_hash);
        if remove {
            fs::remove_file(component).expect("remove component fixture");
        } else {
            fs::write(component, b"corrupt application checkpoint")
                .expect("corrupt component fixture");
        }

        let supervisor = DirectSupervisor::open(&state_root).expect("restart supervisor");
        let RecoveryOutcome::Failed(issue) =
            supervisor.restore_application(checkpoint.epoch_id, ApplicationRestoreMode::Activate)
        else {
            panic!("invalid component must fail restore")
        };
        assert_eq!(issue.code, expected);
    }
}

#[test]
fn restore_reports_future_component_schema_as_unsupported() {
    let fixture = TempDir::new().expect("create future-schema fixture");
    let (state_root, _, _, _, checkpoint) = run_and_checkpoint(&fixture, 51);
    let store = Store::open(state_root.join("state.db")).expect("open metadata store");
    let metadata: String = store
        .connection()
        .query_row(
            "SELECT metadata_json FROM snapshot_components WHERE epoch_id = ?1",
            [checkpoint.epoch_id.to_string()],
            |row| row.get(0),
        )
        .expect("read component metadata");
    let mut metadata: serde_json::Value = serde_json::from_str(&metadata).expect("metadata JSON");
    metadata["schema_version"] = serde_json::json!(2);
    store
        .connection()
        .execute(
            "UPDATE snapshot_components SET metadata_json = ?2 WHERE epoch_id = ?1",
            [
                checkpoint.epoch_id.to_string(),
                serde_json::to_string(&metadata).expect("encode future metadata"),
            ],
        )
        .expect("install future-version fixture");
    drop(store);

    let supervisor = DirectSupervisor::open(&state_root).expect("restart supervisor");
    let RecoveryOutcome::Unsupported(issue) =
        supervisor.restore_application(checkpoint.epoch_id, ApplicationRestoreMode::Activate)
    else {
        panic!("future schema must be unsupported")
    };
    assert_eq!(issue.code, RecoveryCode::SchemaVersion);
}

#[test]
fn restore_rejects_valid_json_that_is_not_the_canonical_context_encoding() {
    let fixture = TempDir::new().expect("create noncanonical fixture");
    let (state_root, _, _, _, checkpoint) = run_and_checkpoint(&fixture, 61);
    let blobs = BlobStore::open(state_root.join("blobs")).expect("open blob store");
    let canonical = blobs
        .read(&checkpoint.component_hash)
        .expect("read canonical checkpoint");
    let context: ApplicationContext =
        serde_json::from_slice(&canonical).expect("decode canonical checkpoint");
    let pretty = serde_json::to_vec_pretty(&context).expect("pretty checkpoint JSON");
    let replacement = blobs
        .put(&pretty, APPLICATION_CONTEXT_MEDIA_TYPE)
        .expect("store noncanonical fixture");
    let store = Store::open(state_root.join("state.db")).expect("open metadata store");
    store
        .connection()
        .execute(
            "INSERT INTO blobs (hash, byte_length, media_type, created_at_unix_ms) \
             VALUES (?1, ?2, ?3, 1)",
            (
                replacement.hash.to_string(),
                i64::try_from(replacement.length).expect("fixture length"),
                APPLICATION_CONTEXT_MEDIA_TYPE,
            ),
        )
        .expect("register noncanonical fixture");
    store
        .connection()
        .execute(
            "UPDATE snapshot_components \
             SET blob_hash = ?2, checksum_sha256 = ?2, byte_length = ?3 \
             WHERE epoch_id = ?1",
            (
                checkpoint.epoch_id.to_string(),
                replacement.hash.to_string(),
                i64::try_from(replacement.length).expect("fixture length"),
            ),
        )
        .expect("point component at noncanonical fixture");
    drop(store);

    let supervisor = DirectSupervisor::open(&state_root).expect("restart supervisor");
    let RecoveryOutcome::Failed(issue) =
        supervisor.restore_application(checkpoint.epoch_id, ApplicationRestoreMode::Activate)
    else {
        panic!("noncanonical context must fail restore")
    };
    assert_eq!(issue.code, RecoveryCode::NonCanonical);
}

#[test]
fn checkpoint_is_explicitly_unsupported_without_a_cooperative_w02_summary() {
    let fixture = TempDir::new().expect("create unsupported fixture");
    let script = fixture.path().join("plain-agent.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '{"payload":{"agent_id":"plain","branch_id":"%s","session_id":"%s"},"protocol_version":1,"sequence":0,"type":"agent.start"}\n' "$EPOCH_BRANCH_ID" "$EPOCH_SESSION_ID"
printf '{"payload":{"outcome":"succeeded","output_hash":null},"protocol_version":1,"sequence":1,"type":"agent.completion"}\n'
printf 'ordinary diagnostic\n' >&2
"#,
    )
    .expect("write plain agent");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("make script executable");
    let manifest = fixture.path().join("plain.toml");
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\nname = \"plain-agent\"\nexecutable = \"{}\"\n",
            script.display()
        ),
    )
    .expect("write plain manifest");
    let supervisor = DirectSupervisor::open(fixture.path().join("state")).expect("open supervisor");
    let run = supervisor.run_manifest(manifest).expect("run plain agent");

    let RecoveryOutcome::Unsupported(issue) =
        supervisor.checkpoint_application(run.session_id, Some(run.branch_id), None)
    else {
        panic!("plain agent checkpoint must be unsupported")
    };
    assert_eq!(issue.code, RecoveryCode::NoCooperativeSafePoint);
}
