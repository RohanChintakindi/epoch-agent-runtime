use std::collections::{BTreeMap, HashSet};

use epoch_blob::{BlobError, BlobHash, BlobStore};
use serde::{Deserialize, Serialize};

use crate::{
    BackendOutcome, CheckpointBackend, CheckpointFailure, CheckpointUnsupported, FailureCode,
    FailureStage, UnsupportedCode,
};

pub const APPLICATION_CONTEXT_SCHEMA_VERSION: u16 = 1;
pub const APPLICATION_CONTEXT_MEDIA_TYPE: &str =
    "application/vnd.epoch.application-context+json;version=1";
const MAX_SIGNED_INTEGER: u64 = i64::MAX as u64;
const MAX_IDENTIFIER_BYTES: usize = 255;
const MAX_NAME_BYTES: usize = 128;
const MAX_COLLECTION_ITEMS: usize = 100_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationContext {
    pub schema_version: u16,
    pub safe_point_id: String,
    pub deterministic_seed: u64,
    pub context_revision: u64,
    pub cursors: ResumeCursors,
    pub model_identifier: String,
    pub tool_registry: BTreeMap<String, String>,
    pub messages: Vec<ObservableMessage>,
    pub pending_tasks: Vec<PendingTask>,
    pub pending_model_request_ids: Vec<String>,
    pub pending_tool_call_ids: Vec<String>,
    pub user_visible_summary_hash: Option<BlobHash>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeCursors {
    pub boundary_sequence: u64,
    pub message_cursor: u64,
    pub tool_cursor: u64,
    pub task_cursor: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservableMessage {
    pub message_id: String,
    pub role: MessageRole,
    pub content_hash: BlobHash,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PendingTask {
    pub task_id: String,
    pub task_type: String,
    pub payload_hash: Option<BlobHash>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationCheckpointMetadata {
    pub safe_point_id: String,
    pub context_revision: u64,
    pub boundary_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationCheckpoint {
    pub component_hash: BlobHash,
    pub byte_length: u64,
    pub schema_version: u16,
    pub metadata: ApplicationCheckpointMetadata,
}

impl ApplicationCheckpoint {
    #[must_use]
    pub const fn from_record(
        component_hash: BlobHash,
        byte_length: u64,
        schema_version: u16,
        metadata: ApplicationCheckpointMetadata,
    ) -> Self {
        Self {
            component_hash,
            byte_length,
            schema_version,
            metadata,
        }
    }
}

#[derive(Debug)]
pub struct ApplicationCheckpointBackend {
    store: BlobStore,
}

impl ApplicationCheckpointBackend {
    #[must_use]
    pub const fn new(store: BlobStore) -> Self {
        Self { store }
    }
}

impl CheckpointBackend for ApplicationCheckpointBackend {
    type State = ApplicationContext;
    type Artifact = ApplicationCheckpoint;

    fn capture(&self, state: &Self::State) -> BackendOutcome<Self::Artifact> {
        if state.schema_version != APPLICATION_CONTEXT_SCHEMA_VERSION {
            return unsupported_schema(state.schema_version);
        }
        if let Err((code, detail)) = validate_context_shape(state) {
            return failed(FailureStage::Capture, code, detail);
        }
        if let Err((code, detail)) = validate_references(&self.store, state) {
            return failed(FailureStage::Capture, code, detail);
        }

        let bytes = match serde_json::to_vec(state) {
            Ok(bytes) => bytes,
            Err(error) => {
                return failed(
                    FailureStage::Capture,
                    FailureCode::InvalidContext,
                    format!("context serialization failed: {error}"),
                );
            }
        };
        let metadata = match self.store.put(&bytes, APPLICATION_CONTEXT_MEDIA_TYPE) {
            Ok(metadata) => metadata,
            Err(error) => {
                return failed(
                    FailureStage::Capture,
                    FailureCode::Storage,
                    error.to_string(),
                );
            }
        };
        BackendOutcome::Supported(ApplicationCheckpoint {
            component_hash: metadata.hash,
            byte_length: metadata.length,
            schema_version: state.schema_version,
            metadata: ApplicationCheckpointMetadata {
                safe_point_id: state.safe_point_id.clone(),
                context_revision: state.context_revision,
                boundary_sequence: state.cursors.boundary_sequence,
            },
        })
    }

    fn restore(&self, artifact: &Self::Artifact) -> BackendOutcome<Self::State> {
        if artifact.schema_version != APPLICATION_CONTEXT_SCHEMA_VERSION {
            return unsupported_schema(artifact.schema_version);
        }
        let bytes = match self.store.read(&artifact.component_hash) {
            Ok(bytes) => bytes,
            Err(BlobError::HashMismatch { .. }) => {
                return failed(
                    FailureStage::Load,
                    FailureCode::Integrity,
                    "checkpoint blob failed SHA-256 verification".to_owned(),
                );
            }
            Err(error) => {
                return failed(FailureStage::Load, FailureCode::Storage, error.to_string());
            }
        };
        let Ok(actual_length) = u64::try_from(bytes.len()) else {
            return failed(
                FailureStage::Load,
                FailureCode::Integrity,
                "checkpoint byte length cannot be represented as u64".to_owned(),
            );
        };
        if actual_length != artifact.byte_length {
            return failed(
                FailureStage::Load,
                FailureCode::Integrity,
                format!(
                    "checkpoint length mismatch: metadata={}, actual={actual_length}",
                    artifact.byte_length
                ),
            );
        }

        let probe: VersionProbe = match serde_json::from_slice(&bytes) {
            Ok(probe) => probe,
            Err(error) => {
                return failed(
                    FailureStage::Load,
                    FailureCode::Decode,
                    format!("context version header is invalid: {error}"),
                );
            }
        };
        if probe.schema_version != APPLICATION_CONTEXT_SCHEMA_VERSION {
            return unsupported_schema(probe.schema_version);
        }
        let context: ApplicationContext = match serde_json::from_slice(&bytes) {
            Ok(context) => context,
            Err(error) => {
                return failed(
                    FailureStage::Load,
                    FailureCode::Decode,
                    format!("context schema is invalid: {error}"),
                );
            }
        };
        if let Err((code, detail)) = validate_context_shape(&context) {
            return failed(FailureStage::Load, code, detail);
        }
        if let Err((code, detail)) = validate_references(&self.store, &context) {
            return failed(FailureStage::Load, code, detail);
        }
        let canonical = match serde_json::to_vec(&context) {
            Ok(canonical) => canonical,
            Err(error) => {
                return failed(
                    FailureStage::Load,
                    FailureCode::Decode,
                    format!("decoded context cannot be re-encoded: {error}"),
                );
            }
        };
        if canonical != bytes {
            return failed(
                FailureStage::Load,
                FailureCode::NonCanonical,
                "context bytes are not the canonical schema encoding".to_owned(),
            );
        }
        if artifact.metadata.safe_point_id != context.safe_point_id
            || artifact.metadata.context_revision != context.context_revision
            || artifact.metadata.boundary_sequence != context.cursors.boundary_sequence
        {
            return failed(
                FailureStage::Load,
                FailureCode::MetadataMismatch,
                "checkpoint metadata does not match serialized context".to_owned(),
            );
        }
        BackendOutcome::Supported(context)
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    schema_version: u16,
}

fn unsupported_schema<T>(found: u16) -> BackendOutcome<T> {
    BackendOutcome::Unsupported(CheckpointUnsupported {
        code: UnsupportedCode::SchemaVersion,
        detail: format!(
            "application context schema {found} is unsupported; expected {APPLICATION_CONTEXT_SCHEMA_VERSION}"
        ),
    })
}

fn failed<T>(stage: FailureStage, code: FailureCode, detail: String) -> BackendOutcome<T> {
    BackendOutcome::Failed(CheckpointFailure {
        stage,
        code,
        detail,
    })
}

fn validate_context_shape(context: &ApplicationContext) -> Result<(), (FailureCode, String)> {
    bounded_identifier("safe_point_id", &context.safe_point_id)?;
    bounded_name("model_identifier", &context.model_identifier)?;
    bounded_integer("deterministic_seed", context.deterministic_seed)?;
    bounded_integer("context_revision", context.context_revision)?;
    bounded_integer(
        "cursors.boundary_sequence",
        context.cursors.boundary_sequence,
    )?;
    bounded_integer("cursors.message_cursor", context.cursors.message_cursor)?;
    bounded_integer("cursors.tool_cursor", context.cursors.tool_cursor)?;
    bounded_integer("cursors.task_cursor", context.cursors.task_cursor)?;
    bounded_collection("tool_registry", context.tool_registry.len())?;
    bounded_collection("messages", context.messages.len())?;
    bounded_collection("pending_tasks", context.pending_tasks.len())?;
    bounded_collection(
        "pending_model_request_ids",
        context.pending_model_request_ids.len(),
    )?;
    bounded_collection("pending_tool_call_ids", context.pending_tool_call_ids.len())?;

    for (name, version) in &context.tool_registry {
        bounded_name("tool_registry.name", name)?;
        bounded_identifier("tool_registry.version", version)?;
    }
    unique_ids(
        "messages.message_id",
        context.messages.iter().map(|message| &message.message_id),
    )?;
    for message in &context.messages {
        bounded_identifier("messages.message_id", &message.message_id)?;
    }
    unique_ids(
        "pending_tasks.task_id",
        context.pending_tasks.iter().map(|task| &task.task_id),
    )?;
    for task in &context.pending_tasks {
        bounded_identifier("pending_tasks.task_id", &task.task_id)?;
        bounded_name("pending_tasks.task_type", &task.task_type)?;
    }
    unique_ids(
        "pending_model_request_ids",
        context.pending_model_request_ids.iter(),
    )?;
    for id in &context.pending_model_request_ids {
        bounded_identifier("pending_model_request_ids", id)?;
    }
    unique_ids(
        "pending_tool_call_ids",
        context.pending_tool_call_ids.iter(),
    )?;
    for id in &context.pending_tool_call_ids {
        bounded_identifier("pending_tool_call_ids", id)?;
    }
    Ok(())
}

fn validate_references(
    store: &BlobStore,
    context: &ApplicationContext,
) -> Result<(), (FailureCode, String)> {
    for hash in context_references(context) {
        match store.read(hash) {
            Ok(_) => {}
            Err(BlobError::NotFound(_)) => {
                return Err((
                    FailureCode::MissingReference,
                    format!("observable context references missing blob {hash}"),
                ));
            }
            Err(BlobError::HashMismatch { .. }) => {
                return Err((
                    FailureCode::Integrity,
                    format!("observable context references corrupt blob {hash}"),
                ));
            }
            Err(error) => return Err((FailureCode::Storage, error.to_string())),
        }
    }
    Ok(())
}

fn context_references(context: &ApplicationContext) -> Vec<&BlobHash> {
    let mut hashes = Vec::new();
    hashes.extend(context.messages.iter().map(|message| &message.content_hash));
    hashes.extend(
        context
            .pending_tasks
            .iter()
            .filter_map(|task| task.payload_hash.as_ref()),
    );
    hashes.extend(context.user_visible_summary_hash.iter());
    hashes
}

fn bounded_identifier(field: &str, value: &str) -> Result<(), (FailureCode, String)> {
    bounded_string(field, value, MAX_IDENTIFIER_BYTES)
}

fn bounded_name(field: &str, value: &str) -> Result<(), (FailureCode, String)> {
    bounded_string(field, value, MAX_NAME_BYTES)
}

fn bounded_string(field: &str, value: &str, maximum: usize) -> Result<(), (FailureCode, String)> {
    if value.trim().is_empty() || value.len() > maximum {
        return Err((
            FailureCode::InvalidContext,
            format!("{field} must contain between 1 and {maximum} UTF-8 bytes"),
        ));
    }
    Ok(())
}

fn bounded_integer(field: &str, value: u64) -> Result<(), (FailureCode, String)> {
    if value > MAX_SIGNED_INTEGER {
        return Err((
            FailureCode::InvalidContext,
            format!("{field} exceeds SQLite's signed integer range"),
        ));
    }
    Ok(())
}

fn bounded_collection(field: &str, length: usize) -> Result<(), (FailureCode, String)> {
    if length > MAX_COLLECTION_ITEMS {
        return Err((
            FailureCode::InvalidContext,
            format!("{field} exceeds {MAX_COLLECTION_ITEMS} entries"),
        ));
    }
    Ok(())
}

fn unique_ids<'a>(
    field: &str,
    identifiers: impl Iterator<Item = &'a String>,
) -> Result<(), (FailureCode, String)> {
    let mut seen = HashSet::new();
    for identifier in identifiers {
        if !seen.insert(identifier) {
            return Err((
                FailureCode::InvalidContext,
                format!("{field} contains duplicate identifier {identifier:?}"),
            ));
        }
    }
    Ok(())
}
