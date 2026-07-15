#![cfg(unix)]

use std::fs;

use epoch_supervisor::{ApplicationRestoreMode, DirectSupervisor, RecoveryOutcome, RestoreScope};
use tempfile::TempDir;

fn supported<T>(outcome: RecoveryOutcome<T>) -> T {
    match outcome {
        RecoveryOutcome::Supported(value) => value,
        RecoveryOutcome::Unsupported(issue) => panic!("unexpected unsupported outcome: {issue:?}"),
        RecoveryOutcome::Failed(issue) => panic!("unexpected failed outcome: {issue:?}"),
    }
}

#[test]
fn three_restart_safe_run_checkpoint_mutate_restore_inspect_cycles_need_no_repair() {
    let fixture = TempDir::new().expect("create recovery fixture");
    let state_root = fixture.path().join("state");

    for seed in [11_u64, 22, 33] {
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
        assert_eq!(
            checkpoint.restore_scope,
            RestoreScope::ApplicationContextOnly
        );
        assert_eq!(checkpoint.context_revision, 1);
        assert_eq!(checkpoint.boundary_sequence, 9);
        drop(supervisor);

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

        let status = supported(restarted.application_status(run.session_id, Some(run.branch_id)));
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
