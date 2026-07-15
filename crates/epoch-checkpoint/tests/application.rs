use std::{collections::BTreeMap, fs};

use epoch_blob::{BlobHash, BlobStore};
use epoch_checkpoint::{
    APPLICATION_CONTEXT_SCHEMA_VERSION, ApplicationCheckpoint, ApplicationCheckpointBackend,
    ApplicationCheckpointMetadata, ApplicationContext, BackendOutcome, CheckpointBackend,
    FailureCode, MessageRole, ObservableMessage, PendingTask, ResumeCursors, UnsupportedCode,
};

struct Fixture {
    _temp: tempfile::TempDir,
    backend: ApplicationCheckpointBackend,
    inspector: BlobStore,
    context: ApplicationContext,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let blob_root = temp.path().join("blobs");
        let producer_store = BlobStore::open(&blob_root).expect("producer blob store");
        let message = producer_store
            .put(b"visible user message", "text/plain")
            .expect("message blob");
        let task = producer_store
            .put(b"observable task payload", "application/json")
            .expect("task blob");
        let summary = producer_store
            .put(b"user-visible summary", "text/plain")
            .expect("summary blob");
        let mut tool_registry = BTreeMap::new();
        tool_registry.insert("network.loopback".to_owned(), "1.0.0".to_owned());
        tool_registry.insert("process.spawn".to_owned(), "1.0.0".to_owned());
        let context = ApplicationContext {
            schema_version: APPLICATION_CONTEXT_SCHEMA_VERSION,
            safe_point_id: "safe-point-full-0000000000005eed".to_owned(),
            deterministic_seed: 0x5eed,
            context_revision: 7,
            cursors: ResumeCursors {
                boundary_sequence: 15,
                message_cursor: 2,
                tool_cursor: 5,
                task_cursor: 3,
            },
            model_identifier: "recorded-model-v1".to_owned(),
            tool_registry,
            messages: vec![ObservableMessage {
                message_id: "message-1".to_owned(),
                role: MessageRole::User,
                content_hash: message.hash,
            }],
            pending_tasks: vec![PendingTask {
                task_id: "task-3".to_owned(),
                task_type: "repository-task".to_owned(),
                payload_hash: Some(task.hash),
            }],
            pending_model_request_ids: Vec::new(),
            pending_tool_call_ids: Vec::new(),
            user_visible_summary_hash: Some(summary.hash),
        };
        let backend = ApplicationCheckpointBackend::new(producer_store);
        let inspector = BlobStore::open(blob_root).expect("inspector blob store");
        Self {
            _temp: temp,
            backend,
            inspector,
            context,
        }
    }

    fn capture(&self) -> ApplicationCheckpoint {
        let BackendOutcome::Supported(artifact) = self.backend.capture(&self.context) else {
            panic!("fixture capture should be supported")
        };
        artifact
    }
}

#[test]
fn capture_is_deterministic_and_restore_preserves_w02_resume_cursors() {
    let fixture = Fixture::new();
    let first = fixture.capture();
    let second = fixture.capture();
    assert_eq!(first, second);
    assert_eq!(first.schema_version, APPLICATION_CONTEXT_SCHEMA_VERSION);
    assert!(first.byte_length > 0);
    assert_eq!(first.metadata.safe_point_id, fixture.context.safe_point_id);

    let BackendOutcome::Supported(restored) = fixture.backend.restore(&first) else {
        panic!("valid checkpoint should restore")
    };
    assert_eq!(restored, fixture.context);
    assert_eq!(restored.cursors.boundary_sequence, 15);
    assert_eq!(restored.cursors.message_cursor, 2);
    assert_eq!(restored.cursors.tool_cursor, 5);
    assert_eq!(restored.cursors.task_cursor, 3);

    let bytes = fixture
        .inspector
        .read(&first.component_hash)
        .expect("stored checkpoint bytes");
    assert_eq!(BlobHash::digest(&bytes), first.component_hash);
    assert_eq!(
        serde_json::to_vec(&fixture.context).expect("canonical context"),
        bytes
    );
    assert!(
        !String::from_utf8(bytes)
            .expect("JSON is UTF-8")
            .contains("chain_of_thought")
    );
}

#[test]
fn capture_fails_when_observable_content_was_not_ingested() {
    let mut fixture = Fixture::new();
    fixture.context.messages[0].content_hash = BlobHash::digest(b"absent");

    let BackendOutcome::Failed(failure) = fixture.backend.capture(&fixture.context) else {
        panic!("missing nested blob must fail capture")
    };
    assert_eq!(failure.code, FailureCode::MissingReference);
}

#[test]
fn corrupted_checkpoint_blob_is_rejected_as_integrity_failure() {
    let fixture = Fixture::new();
    let artifact = fixture.capture();
    fs::write(
        fixture.inspector.blob_path(&artifact.component_hash),
        b"corrupted",
    )
    .expect("corrupt fixture");

    let BackendOutcome::Failed(failure) = fixture.backend.restore(&artifact) else {
        panic!("corruption must fail restore")
    };
    assert_eq!(failure.code, FailureCode::Integrity);
}

#[test]
fn unsupported_context_schema_is_distinct_from_failed_restore() {
    let fixture = Fixture::new();
    let bytes = br#"{"schema_version":2}"#;
    let metadata = fixture
        .inspector
        .put(
            bytes,
            "application/vnd.epoch.application-context+json;version=2",
        )
        .expect("unsupported context blob");
    let artifact = ApplicationCheckpoint::from_record(
        metadata.hash,
        metadata.length,
        2,
        ApplicationCheckpointMetadata {
            safe_point_id: "future".to_owned(),
            context_revision: 0,
            boundary_sequence: 0,
        },
    );

    let BackendOutcome::Unsupported(unsupported) = fixture.backend.restore(&artifact) else {
        panic!("future schema must be unsupported rather than failed")
    };
    assert_eq!(unsupported.code, UnsupportedCode::SchemaVersion);
}

#[test]
fn valid_but_noncanonical_or_metadata_mismatched_context_is_rejected() {
    let fixture = Fixture::new();
    let pretty = serde_json::to_vec_pretty(&fixture.context).expect("pretty context");
    let stored = fixture
        .inspector
        .put(&pretty, "application/json")
        .expect("noncanonical context blob");
    let noncanonical = ApplicationCheckpoint::from_record(
        stored.hash,
        stored.length,
        APPLICATION_CONTEXT_SCHEMA_VERSION,
        ApplicationCheckpointMetadata {
            safe_point_id: fixture.context.safe_point_id.clone(),
            context_revision: fixture.context.context_revision,
            boundary_sequence: fixture.context.cursors.boundary_sequence,
        },
    );
    let BackendOutcome::Failed(failure) = fixture.backend.restore(&noncanonical) else {
        panic!("noncanonical context must fail")
    };
    assert_eq!(failure.code, FailureCode::NonCanonical);

    let captured = fixture.capture();
    let wrong_metadata = ApplicationCheckpoint::from_record(
        captured.component_hash,
        captured.byte_length,
        captured.schema_version,
        ApplicationCheckpointMetadata {
            safe_point_id: "wrong".to_owned(),
            ..captured.metadata
        },
    );
    let BackendOutcome::Failed(failure) = fixture.backend.restore(&wrong_metadata) else {
        panic!("metadata mismatch must fail")
    };
    assert_eq!(failure.code, FailureCode::MetadataMismatch);
}
