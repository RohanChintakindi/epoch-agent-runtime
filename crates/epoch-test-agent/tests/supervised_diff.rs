#![cfg(unix)]

use std::fs;

use epoch_blob::BlobStore;
use epoch_diff::SemanticSection;
use epoch_storage::Store;
use epoch_supervisor::{
    ApplicationCheckpointReport, DirectSupervisor, RecoveryCode, RecoveryOutcome,
};
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
    state_root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().expect("create diff fixture");
        let state_root = directory.path().join("state");
        Self {
            directory,
            state_root,
        }
    }

    fn checkpoint(&self, seed: u64) -> ApplicationCheckpointReport {
        let workspace = self.directory.path().join(format!("workspace-{seed}"));
        let manifest = self.directory.path().join(format!("workload-{seed}.toml"));
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
        .expect("write diff workload");
        let supervisor = DirectSupervisor::open(&self.state_root).expect("open supervisor");
        let run = supervisor.run_manifest(manifest).expect("run W02 workload");
        let RecoveryOutcome::Supported(checkpoint) = supervisor.checkpoint_application(
            run.session_id,
            Some(run.branch_id),
            Some("diff-fixture"),
        ) else {
            panic!("checkpoint must be supported")
        };
        checkpoint
    }
}

#[test]
fn durable_epoch_diff_handles_identical_and_changed_validated_contexts() {
    let fixture = Fixture::new();
    let before = fixture.checkpoint(101);
    let after = fixture.checkpoint(202);
    let restarted = DirectSupervisor::open(&fixture.state_root).expect("restart supervisor");

    let RecoveryOutcome::Supported(identical) =
        restarted.diff_application_epochs(before.epoch_id, before.epoch_id)
    else {
        panic!("identical diff must be supported")
    };
    assert!(identical.diff.identical);
    assert!(identical.diff.changes.is_empty());

    let RecoveryOutcome::Supported(changed) =
        restarted.diff_application_epochs(before.epoch_id, after.epoch_id)
    else {
        panic!("changed diff must be supported")
    };
    assert!(!changed.diff.identical);
    assert!(
        changed
            .diff
            .changes
            .iter()
            .any(|change| change.path == "/deterministic_seed")
    );
    assert_eq!(
        changed
            .diff
            .unsupported_sections
            .iter()
            .map(|section| section.section)
            .collect::<Vec<_>>(),
        [
            SemanticSection::Capabilities,
            SemanticSection::EffectFrontier,
            SemanticSection::WorkspaceFiles,
        ]
    );
}

#[test]
fn epoch_diff_refuses_corrupt_missing_and_future_components() {
    for (seed, mutation, expected_outcome, expected_code) in [
        (301_u64, "corrupt", "failed", RecoveryCode::Integrity),
        (302_u64, "missing", "failed", RecoveryCode::Storage),
        (
            303_u64,
            "future",
            "unsupported",
            RecoveryCode::SchemaVersion,
        ),
    ] {
        let fixture = Fixture::new();
        let checkpoint = fixture.checkpoint(seed);
        match mutation {
            "corrupt" | "missing" => {
                let blobs =
                    BlobStore::open(fixture.state_root.join("blobs")).expect("open blob store");
                let path = blobs.blob_path(&checkpoint.component_hash);
                if mutation == "corrupt" {
                    fs::write(path, b"corrupt diff component").expect("corrupt component");
                } else {
                    fs::remove_file(path).expect("remove component");
                }
            }
            "future" => {
                let store = Store::open(fixture.state_root.join("state.db")).expect("open store");
                let metadata: String = store
                    .connection()
                    .query_row(
                        "SELECT metadata_json FROM snapshot_components WHERE epoch_id = ?1",
                        [checkpoint.epoch_id.to_string()],
                        |row| row.get(0),
                    )
                    .expect("read metadata");
                let mut metadata: serde_json::Value =
                    serde_json::from_str(&metadata).expect("metadata JSON");
                metadata["schema_version"] = serde_json::json!(2);
                store
                    .connection()
                    .execute(
                        "UPDATE snapshot_components SET metadata_json = ?2 WHERE epoch_id = ?1",
                        [
                            checkpoint.epoch_id.to_string(),
                            serde_json::to_string(&metadata).expect("encode metadata"),
                        ],
                    )
                    .expect("install future metadata");
            }
            _ => unreachable!(),
        }

        let supervisor = DirectSupervisor::open(&fixture.state_root).expect("restart supervisor");
        let outcome = supervisor.diff_application_epochs(checkpoint.epoch_id, checkpoint.epoch_id);
        match (expected_outcome, outcome) {
            ("failed", RecoveryOutcome::Failed(issue))
            | ("unsupported", RecoveryOutcome::Unsupported(issue)) => {
                assert_eq!(issue.code, expected_code);
            }
            (_, unexpected) => panic!("unexpected diff outcome: {unexpected:?}"),
        }
    }
}
