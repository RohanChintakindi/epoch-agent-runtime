use epoch_blob::BlobStore;
use epoch_checkpoint::{ApplicationCheckpointBackend, BackendOutcome, CheckpointBackend};
use epoch_test_agent::{Scenario, WorkloadConfig, run_workload};

#[test]
fn completed_w02_run_resumes_from_its_observable_safe_point_cursors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let config = WorkloadConfig::new(0x5eed, Scenario::Full, workspace);
    let mut trace = Vec::new();
    let summary = run_workload(&config, &mut trace).expect("deterministic run");
    let context = summary
        .to_application_context()
        .expect("completed run has a resumable safe point");

    assert_eq!(context.safe_point_id, "safe-point-full-0000000000005eed");
    assert_eq!(context.deterministic_seed, 0x5eed);
    assert_eq!(context.context_revision, 1);
    assert_eq!(context.cursors.boundary_sequence, summary.event_count - 2);
    assert_eq!(context.cursors.message_cursor, 2);
    assert_eq!(
        context.cursors.tool_cursor,
        u64::try_from(summary.state.completed_tools.len()).expect("tool count fits u64")
    );
    assert!(context.pending_model_request_ids.is_empty());
    assert!(context.pending_tool_call_ids.is_empty());

    let store = BlobStore::open(temp.path().join("blobs")).expect("private blob store");
    let backend = ApplicationCheckpointBackend::new(store);
    let BackendOutcome::Supported(artifact) = backend.capture(&context) else {
        panic!("W02 context should checkpoint")
    };
    let BackendOutcome::Supported(restored) = backend.restore(&artifact) else {
        panic!("W02 context should restore")
    };
    assert_eq!(restored.cursors, context.cursors);
    assert_eq!(restored.safe_point_id, context.safe_point_id);
}

#[test]
fn serialized_w02_summary_carries_the_raw_cooperative_checkpoint_context() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config = WorkloadConfig::new(17, Scenario::Files, temp.path().join("workspace"));
    let mut trace = Vec::new();
    let summary = run_workload(&config, &mut trace).expect("run deterministic workload");

    let encoded = serde_json::to_value(&summary).expect("serialize run summary");
    assert_eq!(
        encoded["checkpoint_context"],
        serde_json::to_value(
            summary
                .to_application_context()
                .expect("checkpoint context")
        )
        .expect("serialize checkpoint context")
    );
}
