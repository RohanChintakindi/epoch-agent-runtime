use std::{collections::BTreeMap, fs};

use epoch_blob::{BlobHash, BlobStore};
use epoch_checkpoint::{
    APPLICATION_CONTEXT_SCHEMA_VERSION, ApplicationCheckpoint, ApplicationCheckpointBackend,
    ApplicationCheckpointMetadata, ApplicationContext, BackendOutcome, CheckpointBackend,
    MessageRole, ObservableMessage, PendingTask, ResumeCursors,
};
use epoch_diff::{
    ChangeClassification, DiffErrorKind, DiffSide, SemanticSection,
    UNSUPPORTED_APPLICATION_SECTIONS, diff_application_checkpoints,
};
use serde_json::json;

struct Fixture {
    _temp: tempfile::TempDir,
    backend: ApplicationCheckpointBackend,
    inspector: BlobStore,
    before: ApplicationContext,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("blobs");
        let store = BlobStore::open(&root).expect("blob store");
        let user = store.put(b"user", "text/plain").expect("user blob");
        let task = store
            .put(b"task-one", "application/json")
            .expect("task blob");
        let summary = store
            .put(b"summary", "text/plain")
            .expect("summary blob");
        let before = ApplicationContext {
            schema_version: APPLICATION_CONTEXT_SCHEMA_VERSION,
            safe_point_id: "safe-point-1".to_owned(),
            deterministic_seed: 7,
            context_revision: 1,
            cursors: ResumeCursors {
                boundary_sequence: 10,
                message_cursor: 1,
                tool_cursor: 2,
                task_cursor: 3,
            },
            model_identifier: "recorded-model-v1".to_owned(),
            tool_registry: BTreeMap::from([
                ("file.read".to_owned(), "1".to_owned()),
                ("process.spawn".to_owned(), "1".to_owned()),
            ]),
            messages: vec![ObservableMessage {
                message_id: "message/1".to_owned(),
                role: MessageRole::User,
                content_hash: user.hash,
            }],
            pending_tasks: vec![PendingTask {
                task_id: "task~1".to_owned(),
                task_type: "repository".to_owned(),
                payload_hash: Some(task.hash),
            }],
            pending_model_request_ids: vec!["model-1".to_owned()],
            pending_tool_call_ids: vec!["tool-1".to_owned()],
            user_visible_summary_hash: Some(summary.hash),
        };
        Self {
            _temp: temp,
            backend: ApplicationCheckpointBackend::new(store),
            inspector: BlobStore::open(root).expect("inspector"),
            before,
        }
    }

    fn capture(&self, context: &ApplicationContext) -> ApplicationCheckpoint {
        let BackendOutcome::Supported(checkpoint) = self.backend.capture(context) else {
            panic!("fixture context must capture")
        };
        checkpoint
    }

    fn put(&self, bytes: &[u8]) -> BlobHash {
        self.inspector
            .put(bytes, "application/octet-stream")
            .expect("fixture blob")
            .hash
    }
}

#[test]
fn identical_validated_snapshots_have_no_changes_and_stable_json() {
    let fixture = Fixture::new();
    let checkpoint = fixture.capture(&fixture.before);

    let diff = diff_application_checkpoints(&fixture.backend, &checkpoint, &checkpoint)
        .expect("valid checkpoints");

    assert!(diff.identical);
    assert!(diff.changes.is_empty());
    assert_eq!(diff.schema_version, 1);
    assert_eq!(diff.unsupported_sections, UNSUPPORTED_APPLICATION_SECTIONS);
    assert_eq!(
        serde_json::to_value(&diff).expect("JSON diff"),
        json!({
            "schema_version": 1,
            "before_component_hash": checkpoint.component_hash,
            "after_component_hash": checkpoint.component_hash,
            "identical": true,
            "unsupported_sections": [
                {"section":"capabilities","reason":"not represented by application context schema 1"},
                {"section":"effect_frontier","reason":"not represented by application context schema 1"},
                {"section":"workspace_files","reason":"not represented by application context schema 1"}
            ],
            "changes": []
        })
    );
}

#[test]
fn reports_cursor_task_memory_and_reference_changes_with_json_pointer_paths() {
    let fixture = Fixture::new();
    let mut after = fixture.before.clone();
    after.safe_point_id = "safe-point-2".to_owned();
    after.context_revision = 2;
    after.cursors.boundary_sequence = 11;
    after.cursors.task_cursor = 4;
    after.messages[0].content_hash = fixture.put(b"revised user message");
    after.pending_tasks[0].task_type = "build".to_owned();
    after.pending_tasks[0].payload_hash = Some(fixture.put(b"task revision"));
    after.user_visible_summary_hash = Some(fixture.put(b"new memory summary"));

    let before_checkpoint = fixture.capture(&fixture.before);
    let after_checkpoint = fixture.capture(&after);
    let diff = diff_application_checkpoints(
        &fixture.backend,
        &before_checkpoint,
        &after_checkpoint,
    )
    .expect("valid diff");

    assert!(!diff.identical);
    let paths = diff
        .changes
        .iter()
        .map(|change| change.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            "/context_revision",
            "/cursors/boundary_sequence",
            "/cursors/task_cursor",
            "/messages/message~11/content_hash",
            "/pending_tasks/task~01/payload_hash",
            "/pending_tasks/task~01/task_type",
            "/safe_point_id",
            "/user_visible_summary_hash",
        ]
    );
    assert!(diff.changes.iter().all(|change| {
        change.classification == ChangeClassification::Changed
            && change.before.is_some()
            && change.after.is_some()
    }));
}

#[test]
fn classifies_added_removed_and_changed_keyed_state_deterministically() {
    let fixture = Fixture::new();
    let mut after = fixture.before.clone();
    after.tool_registry.remove("file.read");
    after
        .tool_registry
        .insert("process.spawn".to_owned(), "2".to_owned());
    after
        .tool_registry
        .insert("network.loopback".to_owned(), "1".to_owned());
    after.pending_tasks.clear();
    after.pending_tasks.push(PendingTask {
        task_id: "task-2".to_owned(),
        task_type: "test".to_owned(),
        payload_hash: None,
    });
    after.pending_model_request_ids = vec!["model-2".to_owned()];
    after.pending_tool_call_ids.push("tool-2".to_owned());

    let before_checkpoint = fixture.capture(&fixture.before);
    let after_checkpoint = fixture.capture(&after);
    let diff = diff_application_checkpoints(
        &fixture.backend,
        &before_checkpoint,
        &after_checkpoint,
    )
    .expect("valid diff");

    let summary = diff
        .changes
        .iter()
        .map(|change| {
            (
                change.path.as_str(),
                change.classification,
                change.section,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        summary,
        [
            (
                "/pending_model_request_ids/model-1",
                ChangeClassification::Removed,
                SemanticSection::PendingModelRequests,
            ),
            (
                "/pending_model_request_ids/model-2",
                ChangeClassification::Added,
                SemanticSection::PendingModelRequests,
            ),
            (
                "/pending_tasks/task-2",
                ChangeClassification::Added,
                SemanticSection::PendingTasks,
            ),
            (
                "/pending_tasks/task~01",
                ChangeClassification::Removed,
                SemanticSection::PendingTasks,
            ),
            (
                "/pending_tool_call_ids/tool-2",
                ChangeClassification::Added,
                SemanticSection::PendingToolCalls,
            ),
            (
                "/tool_registry/file.read",
                ChangeClassification::Removed,
                SemanticSection::ToolRegistry,
            ),
            (
                "/tool_registry/network.loopback",
                ChangeClassification::Added,
                SemanticSection::ToolRegistry,
            ),
            (
                "/tool_registry/process.spawn",
                ChangeClassification::Changed,
                SemanticSection::ToolRegistry,
            ),
        ]
    );
}

#[test]
fn output_is_independent_of_input_map_and_entity_order() {
    let fixture = Fixture::new();
    let mut after_a = fixture.before.clone();
    after_a.pending_tasks.push(PendingTask {
        task_id: "z-task".to_owned(),
        task_type: "z".to_owned(),
        payload_hash: None,
    });
    after_a.pending_tasks.push(PendingTask {
        task_id: "a-task".to_owned(),
        task_type: "a".to_owned(),
        payload_hash: None,
    });
    let mut after_b = after_a.clone();
    after_b.pending_tasks.swap(1, 2);

    let before = fixture.capture(&fixture.before);
    let first = fixture.capture(&after_a);
    let second = fixture.capture(&after_b);
    let first_diff = diff_application_checkpoints(&fixture.backend, &before, &first).unwrap();
    let second_diff = diff_application_checkpoints(&fixture.backend, &before, &second).unwrap();

    assert_eq!(first_diff.changes, second_diff.changes);
}

#[test]
fn refuses_corrupted_checkpoint_before_comparison() {
    let fixture = Fixture::new();
    let checkpoint = fixture.capture(&fixture.before);
    fs::write(
        fixture.inspector.blob_path(&checkpoint.component_hash),
        b"corrupt",
    )
    .expect("corrupt checkpoint");

    let error = diff_application_checkpoints(&fixture.backend, &checkpoint, &checkpoint)
        .expect_err("unvalidated bytes must be refused");
    assert_eq!(error.side, DiffSide::Before);
    assert_eq!(error.kind, DiffErrorKind::InvalidCheckpoint);
    assert_eq!(error.code, "integrity");
}

#[test]
fn distinguishes_future_schema_from_invalid_or_unknown_fields() {
    let fixture = Fixture::new();
    let valid = fixture.capture(&fixture.before);
    let future_bytes = br#"{"schema_version":2}"#;
    let future_blob = fixture
        .inspector
        .put(
            future_bytes,
            "application/vnd.epoch.application-context+json;version=2",
        )
        .expect("future blob");
    let future = ApplicationCheckpoint::from_record(
        future_blob.hash,
        future_blob.length,
        2,
        ApplicationCheckpointMetadata {
            safe_point_id: "future".to_owned(),
            context_revision: 0,
            boundary_sequence: 0,
        },
    );
    let unsupported = diff_application_checkpoints(&fixture.backend, &valid, &future)
        .expect_err("future schema is unsupported");
    assert_eq!(unsupported.side, DiffSide::After);
    assert_eq!(unsupported.kind, DiffErrorKind::UnsupportedSchema);
    assert_eq!(unsupported.code, "schema_version");

    let mut unknown = serde_json::to_value(&fixture.before).expect("context JSON");
    unknown["hidden_chain_of_thought"] = json!("must never be accepted");
    let unknown_bytes = serde_json::to_vec(&unknown).expect("unknown JSON");
    let unknown_blob = fixture
        .inspector
        .put(&unknown_bytes, "application/json")
        .expect("unknown blob");
    let unknown_checkpoint = ApplicationCheckpoint::from_record(
        unknown_blob.hash,
        unknown_blob.length,
        APPLICATION_CONTEXT_SCHEMA_VERSION,
        ApplicationCheckpointMetadata {
            safe_point_id: fixture.before.safe_point_id.clone(),
            context_revision: fixture.before.context_revision,
            boundary_sequence: fixture.before.cursors.boundary_sequence,
        },
    );
    let invalid = diff_application_checkpoints(&fixture.backend, &unknown_checkpoint, &valid)
        .expect_err("unknown fields are invalid, not inferred");
    assert_eq!(invalid.side, DiffSide::Before);
    assert_eq!(invalid.kind, DiffErrorKind::InvalidCheckpoint);
    assert_eq!(invalid.code, "decode");
}

#[test]
fn unsupported_sections_are_explicit_not_fabricated_from_context() {
    assert_eq!(
        UNSUPPORTED_APPLICATION_SECTIONS
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
