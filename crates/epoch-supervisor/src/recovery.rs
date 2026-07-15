use std::{
    collections::BTreeMap,
    str::FromStr as _,
    time::{SystemTime, UNIX_EPOCH},
};

use epoch_blob::{BlobHash, BlobStore};
use epoch_checkpoint::{
    APPLICATION_CONTEXT_MEDIA_TYPE, ApplicationCheckpoint, ApplicationCheckpointBackend,
    ApplicationCheckpointMetadata, ApplicationContext, BackendOutcome, CheckpointBackend,
    CheckpointFailure, CheckpointUnsupported, FailureCode, UnsupportedCode,
};
use epoch_core::{BranchId, EpochId, EventActor, EventKind, EventStatus, SessionId};
use epoch_diff::{ApplicationSemanticDiff, DiffError, DiffErrorKind, diff_application_checkpoints};
use epoch_events::{EventQuery, NewEvent};
use epoch_storage::Store;
use rusqlite::{OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::DirectSupervisor;

const APPLICATION_COMPONENT_KIND: &str = "application_context";
const APPLICATION_BACKEND: &str = "cooperative-w02-v1";
const MAX_LABEL_BYTES: usize = 255;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryOutcome<T> {
    Supported(T),
    Unsupported(RecoveryIssue),
    Failed(RecoveryIssue),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RecoveryIssue {
    pub code: RecoveryCode,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCode {
    BranchNameConflict,
    BranchRequired,
    Decode,
    Integrity,
    InvalidCapture,
    InvalidBranchName,
    InvalidContext,
    MetadataMismatch,
    MissingReference,
    NonCanonical,
    NotFound,
    NoCooperativeSafePoint,
    Persistence,
    SchemaVersion,
    Storage,
    UnsupportedMode,
}

impl RecoveryCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BranchNameConflict => "branch_name_conflict",
            Self::BranchRequired => "branch_required",
            Self::Decode => "decode",
            Self::Integrity => "integrity",
            Self::InvalidCapture => "invalid_capture",
            Self::InvalidBranchName => "invalid_branch_name",
            Self::InvalidContext => "invalid_context",
            Self::MetadataMismatch => "metadata_mismatch",
            Self::MissingReference => "missing_reference",
            Self::NonCanonical => "non_canonical",
            Self::NotFound => "not_found",
            Self::NoCooperativeSafePoint => "no_cooperative_safe_point",
            Self::Persistence => "persistence",
            Self::SchemaVersion => "schema_version",
            Self::Storage => "storage",
            Self::UnsupportedMode => "unsupported_mode",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreScope {
    ApplicationContextOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationRestoreMode {
    Activate,
    Inspect,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApplicationCheckpointReport {
    pub epoch_id: EpochId,
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub component_hash: BlobHash,
    pub schema_version: u16,
    pub safe_point_id: String,
    pub context_revision: u64,
    pub boundary_sequence: u64,
    pub restore_scope: RestoreScope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApplicationRestoreReport {
    pub epoch_id: EpochId,
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub component_hash: BlobHash,
    pub context: ApplicationContext,
    pub activated: bool,
    pub process_restored: bool,
    pub workspace_restored: bool,
    pub restore_scope: RestoreScope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApplicationEpochDiffReport {
    pub before_epoch_id: EpochId,
    pub after_epoch_id: EpochId,
    pub diff: ApplicationSemanticDiff,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ApplicationStatusReport {
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub session_state: String,
    pub current_epoch_id: Option<EpochId>,
    pub context: Option<ApplicationContext>,
    pub inherited_from_parent: bool,
    pub restore_scope: RestoreScope,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CapturedRunSummary {
    state: CapturedNormalizedState,
    state_hash: BlobHash,
    normalized_trace_hash: BlobHash,
    event_count: u64,
    checkpoint_context: ApplicationContext,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CapturedNormalizedState {
    seed: u64,
    scenario: String,
    model_response_hash: BlobHash,
    files: BTreeMap<String, BlobHash>,
    memory: Option<CapturedMemoryState>,
    child: Option<CapturedChildState>,
    network: Option<CapturedNetworkState>,
    completed_tools: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CapturedMemoryState {
    bytes: usize,
    content_hash: BlobHash,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CapturedChildState {
    exit_code: i32,
    stdout_hash: BlobHash,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CapturedNetworkState {
    request_hash: BlobHash,
    response_hash: BlobHash,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredApplicationMetadata {
    schema_version: u16,
    safe_point_id: String,
    context_revision: u64,
    boundary_sequence: u64,
    restore_scope: RestoreScope,
    label: Option<String>,
}

struct LoadedApplicationEpoch {
    epoch_id: EpochId,
    session_id: SessionId,
    branch_id: BranchId,
    artifact: ApplicationCheckpoint,
    effect_frontier: u64,
}

pub(crate) struct ValidatedApplicationSource {
    pub epoch_id: EpochId,
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub component_hash: BlobHash,
    pub context: ApplicationContext,
    pub effect_frontier: u64,
}

struct StoredApplicationBranch {
    session_state: String,
    parent_branch_id: Option<String>,
    fork_epoch_id: Option<String>,
    fork_point_sequence: Option<i64>,
    fork_component_hash: Option<String>,
}

struct ValidatedBoundary {
    safe_sequence: u64,
    safe_point_id: String,
    context_revision: u64,
}

enum RecoveryRejection {
    Unsupported(RecoveryIssue),
    Failed(RecoveryIssue),
}

impl RecoveryRejection {
    fn into_outcome<T>(self) -> RecoveryOutcome<T> {
        match self {
            Self::Unsupported(issue) => RecoveryOutcome::Unsupported(issue),
            Self::Failed(issue) => RecoveryOutcome::Failed(issue),
        }
    }
}

impl DirectSupervisor {
    /// Captures a completed W02 cooperative safe point into a durable application-only epoch.
    #[must_use]
    pub fn checkpoint_application(
        &self,
        session_id: SessionId,
        requested_branch: Option<BranchId>,
        label: Option<&str>,
    ) -> RecoveryOutcome<ApplicationCheckpointReport> {
        if label.is_some_and(|value| value.len() > MAX_LABEL_BYTES || value.contains('\0')) {
            return failed(
                RecoveryCode::InvalidCapture,
                format!("checkpoint label must be at most {MAX_LABEL_BYTES} bytes"),
            );
        }
        let branch_id = match self.resolve_branch(session_id, requested_branch) {
            Ok(branch_id) => branch_id,
            Err(rejection) => return rejection.into_outcome(),
        };
        let context = match self.capture_w02_context(session_id, branch_id) {
            Ok(context) => context,
            Err(rejection) => return rejection.into_outcome(),
        };
        let backend = match self.application_backend() {
            Ok(backend) => backend,
            Err(issue) => return RecoveryOutcome::Failed(issue),
        };
        let artifact = match backend.capture(&context) {
            BackendOutcome::Supported(artifact) => artifact,
            BackendOutcome::Unsupported(issue) => {
                return RecoveryOutcome::Unsupported(map_unsupported(issue));
            }
            BackendOutcome::Failed(issue) => return RecoveryOutcome::Failed(map_failure(issue)),
        };
        let epoch_id = match self.persist_checkpoint(
            session_id,
            branch_id,
            &artifact,
            label.map(str::to_owned),
        ) {
            Ok(epoch_id) => epoch_id,
            Err(issue) => return RecoveryOutcome::Failed(issue),
        };

        RecoveryOutcome::Supported(ApplicationCheckpointReport {
            epoch_id,
            session_id,
            branch_id,
            component_hash: artifact.component_hash,
            schema_version: artifact.schema_version,
            safe_point_id: artifact.metadata.safe_point_id,
            context_revision: artifact.metadata.context_revision,
            boundary_sequence: artifact.metadata.boundary_sequence,
            restore_scope: RestoreScope::ApplicationContextOnly,
        })
    }

    /// Validates and optionally activates a durable application-only checkpoint.
    #[must_use]
    pub fn restore_application(
        &self,
        epoch_id: EpochId,
        mode: ApplicationRestoreMode,
    ) -> RecoveryOutcome<ApplicationRestoreReport> {
        let loaded = match self.load_epoch(epoch_id) {
            Ok(loaded) => loaded,
            Err(rejection) => return rejection.into_outcome(),
        };
        let backend = match self.application_backend() {
            Ok(backend) => backend,
            Err(issue) => return RecoveryOutcome::Failed(issue),
        };
        let context = match backend.restore(&loaded.artifact) {
            BackendOutcome::Supported(context) => context,
            BackendOutcome::Unsupported(issue) => {
                return RecoveryOutcome::Unsupported(map_unsupported(issue));
            }
            BackendOutcome::Failed(issue) => return RecoveryOutcome::Failed(map_failure(issue)),
        };
        let activated = mode == ApplicationRestoreMode::Activate;
        if activated && let Err(issue) = self.record_activation(&loaded, &context) {
            return RecoveryOutcome::Failed(issue);
        }

        RecoveryOutcome::Supported(ApplicationRestoreReport {
            epoch_id,
            session_id: loaded.session_id,
            branch_id: loaded.branch_id,
            component_hash: loaded.artifact.component_hash,
            context,
            activated,
            process_restored: false,
            workspace_restored: false,
            restore_scope: RestoreScope::ApplicationContextOnly,
        })
    }

    /// Compares two durable application epochs after validating both checkpoint components.
    #[must_use]
    pub fn diff_application_epochs(
        &self,
        before_epoch_id: EpochId,
        after_epoch_id: EpochId,
    ) -> RecoveryOutcome<ApplicationEpochDiffReport> {
        let before = match self.load_epoch(before_epoch_id) {
            Ok(loaded) => loaded,
            Err(rejection) => return rejection.into_outcome(),
        };
        let after = match self.load_epoch(after_epoch_id) {
            Ok(loaded) => loaded,
            Err(rejection) => return rejection.into_outcome(),
        };
        let backend = match self.application_backend() {
            Ok(backend) => backend,
            Err(issue) => return RecoveryOutcome::Failed(issue),
        };
        match diff_application_checkpoints(&backend, &before.artifact, &after.artifact) {
            Ok(diff) => RecoveryOutcome::Supported(ApplicationEpochDiffReport {
                before_epoch_id,
                after_epoch_id,
                diff,
            }),
            Err(error) => map_diff_error(&error),
        }
    }

    pub(crate) fn validated_application_source(
        &self,
        epoch_id: EpochId,
    ) -> RecoveryOutcome<ValidatedApplicationSource> {
        let loaded = match self.load_epoch(epoch_id) {
            Ok(loaded) => loaded,
            Err(rejection) => return rejection.into_outcome(),
        };
        let backend = match self.application_backend() {
            Ok(backend) => backend,
            Err(issue) => return RecoveryOutcome::Failed(issue),
        };
        let context = match backend.restore(&loaded.artifact) {
            BackendOutcome::Supported(context) => context,
            BackendOutcome::Unsupported(issue) => {
                return RecoveryOutcome::Unsupported(map_unsupported(issue));
            }
            BackendOutcome::Failed(issue) => return RecoveryOutcome::Failed(map_failure(issue)),
        };
        RecoveryOutcome::Supported(ValidatedApplicationSource {
            epoch_id,
            session_id: loaded.session_id,
            branch_id: loaded.branch_id,
            component_hash: loaded.artifact.component_hash,
            context,
            effect_frontier: loaded.effect_frontier,
        })
    }

    /// Inspects the latest activated application context for one session branch.
    #[must_use]
    pub fn application_status(
        &self,
        session_id: SessionId,
        requested_branch: Option<BranchId>,
    ) -> RecoveryOutcome<ApplicationStatusReport> {
        let branch_id = match self.resolve_branch(session_id, requested_branch) {
            Ok(branch_id) => branch_id,
            Err(rejection) => return rejection.into_outcome(),
        };
        let store = match Store::open(&self.database_path) {
            Ok(store) => store,
            Err(error) => return failed(RecoveryCode::Persistence, error.to_string()),
        };
        let stored_branch = match store.connection().query_row(
            "SELECT s.state, b.parent_branch_id, b.fork_epoch_id, b.fork_point_sequence, \
                    b.fork_component_hash \
             FROM sessions s JOIN branches b ON b.session_id = s.id \
             WHERE s.id = ?1 AND b.id = ?2",
            params![session_id.to_string(), branch_id.to_string()],
            |row| {
                Ok(StoredApplicationBranch {
                    session_state: row.get(0)?,
                    parent_branch_id: row.get(1)?,
                    fork_epoch_id: row.get(2)?,
                    fork_point_sequence: row.get(3)?,
                    fork_component_hash: row.get(4)?,
                })
            },
        ) {
            Ok(value) => value,
            Err(error) => return failed(RecoveryCode::Persistence, error.to_string()),
        };
        drop(store);

        let kind = match EventKind::new("application.context_restored") {
            Ok(kind) => kind,
            Err(error) => return failed(RecoveryCode::Persistence, error.to_string()),
        };
        let events = match self.journal.query(&EventQuery {
            session_id,
            branch_id: Some(branch_id),
            kind: Some(kind),
            sequence: None,
        }) {
            Ok(events) => events,
            Err(error) => return failed(RecoveryCode::Persistence, error.to_string()),
        };
        let Some(event) = events.last() else {
            return self.status_without_activation(session_id, branch_id, stored_branch);
        };
        let payload = match self.journal.read_payload(event) {
            Ok(payload) => payload,
            Err(error) => return failed(RecoveryCode::Persistence, error.to_string()),
        };
        let Some(epoch_id) = payload
            .get("epoch_id")
            .and_then(Value::as_str)
            .and_then(|value| EpochId::from_str(value).ok())
        else {
            return failed(
                RecoveryCode::MetadataMismatch,
                "restored application event has no valid epoch ID".to_owned(),
            );
        };
        let restored = match self.restore_application(epoch_id, ApplicationRestoreMode::Inspect) {
            RecoveryOutcome::Supported(restored) => restored,
            RecoveryOutcome::Unsupported(issue) => return RecoveryOutcome::Unsupported(issue),
            RecoveryOutcome::Failed(issue) => return RecoveryOutcome::Failed(issue),
        };
        if restored.session_id != session_id || restored.branch_id != branch_id {
            return failed(
                RecoveryCode::MetadataMismatch,
                "restored application event points outside its branch".to_owned(),
            );
        }
        RecoveryOutcome::Supported(ApplicationStatusReport {
            session_id,
            branch_id,
            session_state: stored_branch.session_state,
            current_epoch_id: Some(epoch_id),
            context: Some(restored.context),
            inherited_from_parent: false,
            restore_scope: RestoreScope::ApplicationContextOnly,
        })
    }

    fn status_without_activation(
        &self,
        session_id: SessionId,
        branch_id: BranchId,
        branch: StoredApplicationBranch,
    ) -> RecoveryOutcome<ApplicationStatusReport> {
        let lineage = (
            branch.parent_branch_id,
            branch.fork_epoch_id,
            branch.fork_point_sequence,
            branch.fork_component_hash,
        );
        let (Some(parent), Some(epoch), Some(point), Some(component)) = lineage else {
            if lineage != (None, None, None, None) {
                return failed(
                    RecoveryCode::MetadataMismatch,
                    "branch has partial fork lineage".to_owned(),
                );
            }
            return RecoveryOutcome::Supported(ApplicationStatusReport {
                session_id,
                branch_id,
                session_state: branch.session_state,
                current_epoch_id: None,
                context: None,
                inherited_from_parent: false,
                restore_scope: RestoreScope::ApplicationContextOnly,
            });
        };
        let epoch_id = match EpochId::from_str(&epoch) {
            Ok(epoch_id) => epoch_id,
            Err(error) => return failed(RecoveryCode::MetadataMismatch, error.to_string()),
        };
        let source = match self.validated_application_source(epoch_id) {
            RecoveryOutcome::Supported(source) => source,
            RecoveryOutcome::Unsupported(issue) => return RecoveryOutcome::Unsupported(issue),
            RecoveryOutcome::Failed(issue) => return RecoveryOutcome::Failed(issue),
        };
        let expected_point = match u64::try_from(point) {
            Ok(point) => point,
            Err(error) => return failed(RecoveryCode::MetadataMismatch, error.to_string()),
        };
        if source.session_id != session_id
            || source.branch_id.to_string() != parent
            || source.component_hash.to_string() != component
            || source.context.cursors.boundary_sequence != expected_point
        {
            return failed(
                RecoveryCode::MetadataMismatch,
                "fork lineage does not match its validated application checkpoint".to_owned(),
            );
        }
        RecoveryOutcome::Supported(ApplicationStatusReport {
            session_id,
            branch_id,
            session_state: branch.session_state,
            current_epoch_id: Some(epoch_id),
            context: Some(source.context),
            inherited_from_parent: true,
            restore_scope: RestoreScope::ApplicationContextOnly,
        })
    }

    fn application_backend(&self) -> Result<ApplicationCheckpointBackend, RecoveryIssue> {
        BlobStore::open(&self.blob_root)
            .map(ApplicationCheckpointBackend::new)
            .map_err(|error| issue(RecoveryCode::Storage, error.to_string()))
    }

    fn resolve_branch(
        &self,
        session_id: SessionId,
        requested_branch: Option<BranchId>,
    ) -> Result<BranchId, RecoveryRejection> {
        let store = Store::open(&self.database_path)
            .map_err(|error| failed_rejection(RecoveryCode::Persistence, error.to_string()))?;
        let mut statement = store
            .connection()
            .prepare("SELECT id FROM branches WHERE session_id = ?1 ORDER BY id")
            .map_err(|error| failed_rejection(RecoveryCode::Persistence, error.to_string()))?;
        let rows = statement
            .query_map([session_id.to_string()], |row| row.get::<_, String>(0))
            .map_err(|error| failed_rejection(RecoveryCode::Persistence, error.to_string()))?;
        let branches = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| failed_rejection(RecoveryCode::Persistence, error.to_string()))?;
        if branches.is_empty() {
            return Err(failed_rejection(
                RecoveryCode::NotFound,
                format!("session {session_id} was not found"),
            ));
        }
        if let Some(requested) = requested_branch {
            if branches
                .iter()
                .any(|branch| branch == &requested.to_string())
            {
                return Ok(requested);
            }
            return Err(failed_rejection(
                RecoveryCode::NotFound,
                format!("branch {requested} does not belong to session {session_id}"),
            ));
        }
        if branches.len() != 1 {
            return Err(unsupported_rejection(
                RecoveryCode::BranchRequired,
                format!(
                    "session {session_id} has {} branches; select one explicitly",
                    branches.len()
                ),
            ));
        }
        BranchId::from_str(&branches[0]).map_err(|error| {
            failed_rejection(
                RecoveryCode::Persistence,
                format!("stored branch ID is invalid: {error}"),
            )
        })
    }

    fn capture_w02_context(
        &self,
        session_id: SessionId,
        branch_id: BranchId,
    ) -> Result<ApplicationContext, RecoveryRejection> {
        let stderr_kind = EventKind::new("process.stderr")
            .map_err(|error| failed_rejection(RecoveryCode::Persistence, error.to_string()))?;
        let events = self
            .journal
            .query(&EventQuery {
                session_id,
                branch_id: Some(branch_id),
                kind: Some(stderr_kind),
                sequence: None,
            })
            .map_err(|error| failed_rejection(RecoveryCode::Persistence, error.to_string()))?;
        let Some(event) = events.last() else {
            return Err(unsupported_rejection(
                RecoveryCode::NoCooperativeSafePoint,
                "run did not publish a captured W02 checkpoint summary".to_owned(),
            ));
        };
        let payload = self
            .journal
            .read_payload(event)
            .map_err(|error| failed_rejection(RecoveryCode::Persistence, error.to_string()))?;
        let bytes: Vec<u8> = serde_json::from_value(payload["bytes"].clone()).map_err(|_| {
            unsupported_rejection(
                RecoveryCode::NoCooperativeSafePoint,
                "captured stderr is not a W02 checkpoint summary".to_owned(),
            )
        })?;
        let captured: CapturedRunSummary = serde_json::from_slice(&bytes).map_err(|_| {
            unsupported_rejection(
                RecoveryCode::NoCooperativeSafePoint,
                "captured stderr is not a W02 checkpoint summary".to_owned(),
            )
        })?;
        if let Err(detail) = self.validate_capture(session_id, branch_id, &captured) {
            return Err(failed_rejection(RecoveryCode::InvalidCapture, detail));
        }
        Ok(captured.checkpoint_context)
    }

    fn validate_capture(
        &self,
        session_id: SessionId,
        branch_id: BranchId,
        captured: &CapturedRunSummary,
    ) -> Result<(), String> {
        let state_bytes = serde_json::to_vec(&captured.state)
            .map_err(|error| format!("captured W02 state cannot be encoded: {error}"))?;
        let computed_state_hash = BlobHash::digest(&state_bytes);
        if computed_state_hash != captured.state_hash {
            return Err("captured W02 state hash does not match its raw state bytes".to_owned());
        }
        let boundary = self.validate_boundary(session_id, branch_id, &computed_state_hash)?;
        let context = &captured.checkpoint_context;
        if context.safe_point_id != boundary.safe_point_id {
            return Err("checkpoint context safe-point ID does not match the boundary".to_owned());
        }
        if context.context_revision != boundary.context_revision {
            return Err("checkpoint revision does not match the final context update".to_owned());
        }
        if context.cursors.boundary_sequence != boundary.safe_sequence {
            return Err("checkpoint boundary cursor does not match the safe point".to_owned());
        }
        if context.deterministic_seed != captured.state.seed {
            return Err("checkpoint seed does not match captured W02 state".to_owned());
        }
        if context.cursors.message_cursor != 2 {
            return Err(
                "checkpoint model cursor does not match the completed W02 exchange".to_owned(),
            );
        }
        if captured.event_count.checked_sub(2) != Some(boundary.safe_sequence) {
            return Err(
                "checkpoint event count does not end at completion after safe point".to_owned(),
            );
        }
        let completed = u64::try_from(captured.state.completed_tools.len())
            .map_err(|error| error.to_string())?;
        if context.cursors.tool_cursor != completed || context.cursors.task_cursor != completed {
            return Err("checkpoint tool/task cursors do not match captured W02 state".to_owned());
        }
        let expected_tools = captured
            .state
            .completed_tools
            .iter()
            .map(|tool| (tool.clone(), "fixture-v1".to_owned()))
            .collect::<BTreeMap<_, _>>();
        if context.tool_registry != expected_tools {
            return Err("checkpoint tool registry does not match captured W02 state".to_owned());
        }
        Ok(())
    }

    fn validate_boundary(
        &self,
        session_id: SessionId,
        branch_id: BranchId,
        computed_state_hash: &BlobHash,
    ) -> Result<ValidatedBoundary, String> {
        let safe_kind = EventKind::new("safe_point").map_err(|error| error.to_string())?;
        let safe_events = self
            .journal
            .query(&EventQuery {
                session_id,
                branch_id: Some(branch_id),
                kind: Some(safe_kind),
                sequence: None,
            })
            .map_err(|error| error.to_string())?;
        let safe_event = safe_events
            .last()
            .ok_or_else(|| "run has no durable cooperative safe point".to_owned())?;
        let safe_payload = self
            .journal
            .read_payload(safe_event)
            .map_err(|error| error.to_string())?;
        let safe_sequence = safe_payload["sequence"]
            .as_u64()
            .ok_or_else(|| "safe point has no protocol sequence".to_owned())?;
        let safe_point_id = safe_payload["payload"]["safe_point_id"]
            .as_str()
            .ok_or_else(|| "safe point has no identifier".to_owned())?;
        let safe_hash = safe_payload["payload"]["context_hash"]
            .as_str()
            .and_then(|value| BlobHash::from_str(value).ok())
            .ok_or_else(|| "safe point has no canonical context hash".to_owned())?;
        if &safe_hash != computed_state_hash {
            return Err("safe point hash does not match captured raw W02 state".to_owned());
        }
        let context_kind = EventKind::new("context.update").map_err(|error| error.to_string())?;
        let context_events = self
            .journal
            .query(&EventQuery {
                session_id,
                branch_id: Some(branch_id),
                kind: Some(context_kind),
                sequence: None,
            })
            .map_err(|error| error.to_string())?;
        let context_event = context_events
            .last()
            .ok_or_else(|| "run has no durable final context update".to_owned())?;
        if context_event.sequence >= safe_event.sequence {
            return Err("final context update is not causally before the safe point".to_owned());
        }
        let update_payload = self
            .journal
            .read_payload(context_event)
            .map_err(|error| error.to_string())?;
        let update_revision = update_payload["payload"]["revision"]
            .as_u64()
            .ok_or_else(|| "context update has no revision".to_owned())?;
        let update_hash = update_payload["payload"]["context_hash"]
            .as_str()
            .and_then(|value| BlobHash::from_str(value).ok())
            .ok_or_else(|| "context update has no canonical context hash".to_owned())?;
        if &update_hash != computed_state_hash {
            return Err("final context update does not match captured raw W02 state".to_owned());
        }
        Ok(ValidatedBoundary {
            safe_sequence,
            safe_point_id: safe_point_id.to_owned(),
            context_revision: update_revision,
        })
    }

    fn persist_checkpoint(
        &self,
        session_id: SessionId,
        branch_id: BranchId,
        artifact: &ApplicationCheckpoint,
        label: Option<String>,
    ) -> Result<EpochId, RecoveryIssue> {
        let timestamp = unix_ms().map_err(|detail| issue(RecoveryCode::Persistence, detail))?;
        let mut store = Store::open(&self.database_path)
            .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
        ensure_completed_scope(&transaction, session_id, branch_id)?;
        register_component_blob(&transaction, artifact, timestamp)?;

        let (sequence, parent_epoch_id) = next_epoch(&transaction, branch_id)?;
        let policy_revision = transaction
            .query_row(
                "SELECT policy_revision FROM sessions WHERE id = ?1",
                [session_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
        let epoch_id = EpochId::new();
        transaction
            .execute(
                "INSERT INTO epochs \
                 (id, session_id, branch_id, parent_epoch_id, sequence, status, backend, \
                  policy_revision, effect_frontier, created_at_unix_ms, committed_at_unix_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'committed', ?6, ?7, 0, ?8, ?8)",
                params![
                    epoch_id.to_string(),
                    session_id.to_string(),
                    branch_id.to_string(),
                    parent_epoch_id,
                    sequence,
                    APPLICATION_BACKEND,
                    policy_revision,
                    timestamp,
                ],
            )
            .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
        let stored_metadata = StoredApplicationMetadata {
            schema_version: artifact.schema_version,
            safe_point_id: artifact.metadata.safe_point_id.clone(),
            context_revision: artifact.metadata.context_revision,
            boundary_sequence: artifact.metadata.boundary_sequence,
            restore_scope: RestoreScope::ApplicationContextOnly,
            label,
        };
        let metadata_json = serde_json::to_string(&stored_metadata)
            .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
        transaction
            .execute(
                "INSERT INTO snapshot_components \
                 (epoch_id, kind, status, backend, blob_hash, checksum_sha256, byte_length, \
                  metadata_json, staged_at_unix_ms, committed_at_unix_ms) \
                 VALUES (?1, ?2, 'committed', ?3, ?4, ?4, ?5, ?6, ?7, ?7)",
                params![
                    epoch_id.to_string(),
                    APPLICATION_COMPONENT_KIND,
                    APPLICATION_BACKEND,
                    artifact.component_hash.to_string(),
                    i64::try_from(artifact.byte_length)
                        .map_err(|error| issue(RecoveryCode::Persistence, error.to_string(),))?,
                    metadata_json,
                    timestamp,
                ],
            )
            .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
        transaction
            .commit()
            .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
        Ok(epoch_id)
    }

    fn load_epoch(&self, epoch_id: EpochId) -> Result<LoadedApplicationEpoch, RecoveryRejection> {
        let store = Store::open(&self.database_path)
            .map_err(|error| failed_rejection(RecoveryCode::Persistence, error.to_string()))?;
        let row = store
            .connection()
            .query_row(
                "SELECT e.session_id, e.branch_id, e.status, e.effect_frontier, c.status, c.backend, \
                        c.blob_hash, c.checksum_sha256, c.byte_length, c.metadata_json, \
                        b.byte_length, b.media_type \
                 FROM epochs e \
                 JOIN snapshot_components c ON c.epoch_id = e.id \
                 JOIN blobs b ON b.hash = c.blob_hash \
                 WHERE e.id = ?1 AND c.kind = ?2",
                params![epoch_id.to_string(), APPLICATION_COMPONENT_KIND],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, String>(11)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| failed_rejection(RecoveryCode::Persistence, error.to_string()))?;
        let Some((
            session,
            branch,
            epoch_status,
            effect_frontier,
            component_status,
            backend,
            blob_hash,
            checksum,
            component_length,
            metadata_json,
            registered_length,
            media_type,
        )) = row
        else {
            return Err(failed_rejection(
                RecoveryCode::NotFound,
                format!("committed application epoch {epoch_id} was not found"),
            ));
        };
        if epoch_status != "committed"
            || component_status != "committed"
            || backend != APPLICATION_BACKEND
            || checksum != blob_hash
            || component_length != registered_length
            || media_type != APPLICATION_CONTEXT_MEDIA_TYPE
        {
            return Err(failed_rejection(
                RecoveryCode::MetadataMismatch,
                "application epoch metadata is inconsistent".to_owned(),
            ));
        }
        let metadata = decode_stored_application_metadata(&metadata_json)?;
        let session_id = SessionId::from_str(&session)
            .map_err(|error| failed_rejection(RecoveryCode::MetadataMismatch, error.to_string()))?;
        let branch_id = BranchId::from_str(&branch)
            .map_err(|error| failed_rejection(RecoveryCode::MetadataMismatch, error.to_string()))?;
        let component_hash = BlobHash::from_str(&blob_hash)
            .map_err(|error| failed_rejection(RecoveryCode::MetadataMismatch, error.to_string()))?;
        let byte_length = u64::try_from(component_length)
            .map_err(|error| failed_rejection(RecoveryCode::MetadataMismatch, error.to_string()))?;
        let effect_frontier = u64::try_from(effect_frontier)
            .map_err(|error| failed_rejection(RecoveryCode::MetadataMismatch, error.to_string()))?;
        Ok(LoadedApplicationEpoch {
            epoch_id,
            session_id,
            branch_id,
            artifact: ApplicationCheckpoint::from_record(
                component_hash,
                byte_length,
                metadata.schema_version,
                ApplicationCheckpointMetadata {
                    safe_point_id: metadata.safe_point_id,
                    context_revision: metadata.context_revision,
                    boundary_sequence: metadata.boundary_sequence,
                },
            ),
            effect_frontier,
        })
    }

    fn record_activation(
        &self,
        loaded: &LoadedApplicationEpoch,
        context: &ApplicationContext,
    ) -> Result<(), RecoveryIssue> {
        let kind = EventKind::new("application.context_restored")
            .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
        self.journal
            .append(NewEvent {
                session_id: loaded.session_id,
                branch_id: loaded.branch_id,
                epoch_id: Some(loaded.epoch_id),
                causal_parent: None,
                monotonic_ns: 0,
                occurred_at_unix_ms: unix_ms()
                    .map_err(|detail| issue(RecoveryCode::Persistence, detail))?,
                actor: EventActor::Supervisor,
                kind,
                input_hash: None,
                output_hash: Some(loaded.artifact.component_hash.clone()),
                status: EventStatus::Succeeded,
                payload: json!({
                    "epoch_id": loaded.epoch_id,
                    "restore_scope": RestoreScope::ApplicationContextOnly,
                    "safe_point_id": context.safe_point_id,
                    "context_revision": context.context_revision,
                    "boundary_sequence": context.cursors.boundary_sequence,
                    "process_restored": false,
                    "workspace_restored": false,
                }),
            })
            .map(|_| ())
            .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))
    }
}

fn decode_stored_application_metadata(
    encoded: &str,
) -> Result<StoredApplicationMetadata, RecoveryRejection> {
    let metadata: StoredApplicationMetadata = serde_json::from_str(encoded).map_err(|error| {
        failed_rejection(
            RecoveryCode::MetadataMismatch,
            format!("invalid component metadata: {error}"),
        )
    })?;
    if metadata.restore_scope != RestoreScope::ApplicationContextOnly {
        return Err(failed_rejection(
            RecoveryCode::MetadataMismatch,
            "application epoch has an invalid restore scope".to_owned(),
        ));
    }
    Ok(metadata)
}

fn ensure_completed_scope(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    branch_id: BranchId,
) -> Result<(), RecoveryIssue> {
    let state = transaction
        .query_row(
            "SELECT s.state, b.state \
             FROM sessions s JOIN branches b ON b.session_id = s.id \
             WHERE s.id = ?1 AND b.id = ?2",
            params![session_id.to_string(), branch_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
    match state {
        Some((session, branch)) if session == "completed" && branch == "completed" => Ok(()),
        Some((session, branch)) => Err(issue(
            RecoveryCode::InvalidCapture,
            format!(
                "checkpoint requires completed W02 state, got session={session} branch={branch}"
            ),
        )),
        None => Err(issue(
            RecoveryCode::NotFound,
            "session branch was not found".to_owned(),
        )),
    }
}

fn register_component_blob(
    transaction: &Transaction<'_>,
    artifact: &ApplicationCheckpoint,
    timestamp: i64,
) -> Result<(), RecoveryIssue> {
    let length = i64::try_from(artifact.byte_length)
        .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
    transaction
        .execute(
            "INSERT INTO blobs (hash, byte_length, media_type, created_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(hash) DO NOTHING",
            params![
                artifact.component_hash.to_string(),
                length,
                APPLICATION_CONTEXT_MEDIA_TYPE,
                timestamp,
            ],
        )
        .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
    let matches: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM blobs \
             WHERE hash = ?1 AND byte_length = ?2 AND media_type = ?3)",
            params![
                artifact.component_hash.to_string(),
                length,
                APPLICATION_CONTEXT_MEDIA_TYPE,
            ],
            |row| row.get(0),
        )
        .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
    if matches {
        Ok(())
    } else {
        Err(issue(
            RecoveryCode::MetadataMismatch,
            "registered blob metadata conflicts with checkpoint component".to_owned(),
        ))
    }
}

fn next_epoch(
    transaction: &Transaction<'_>,
    branch_id: BranchId,
) -> Result<(i64, Option<String>), RecoveryIssue> {
    let latest = transaction
        .query_row(
            "SELECT id, sequence FROM epochs \
             WHERE branch_id = ?1 AND status = 'committed' \
             ORDER BY sequence DESC LIMIT 1",
            [branch_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|error| issue(RecoveryCode::Persistence, error.to_string()))?;
    latest.map_or(Ok((0, None)), |(parent, sequence)| {
        sequence
            .checked_add(1)
            .map(|next| (next, Some(parent)))
            .ok_or_else(|| {
                issue(
                    RecoveryCode::Persistence,
                    "epoch sequence overflow".to_owned(),
                )
            })
    })
}

fn map_unsupported(value: CheckpointUnsupported) -> RecoveryIssue {
    issue(
        match value.code {
            UnsupportedCode::SchemaVersion => RecoveryCode::SchemaVersion,
            UnsupportedCode::CooperationRequired => RecoveryCode::NoCooperativeSafePoint,
        },
        value.detail,
    )
}

fn map_failure(value: CheckpointFailure) -> RecoveryIssue {
    issue(
        match value.code {
            FailureCode::InvalidContext => RecoveryCode::InvalidContext,
            FailureCode::MissingReference => RecoveryCode::MissingReference,
            FailureCode::Storage => RecoveryCode::Storage,
            FailureCode::Integrity => RecoveryCode::Integrity,
            FailureCode::Decode => RecoveryCode::Decode,
            FailureCode::NonCanonical => RecoveryCode::NonCanonical,
            FailureCode::MetadataMismatch => RecoveryCode::MetadataMismatch,
        },
        value.detail,
    )
}

fn map_diff_error<T>(error: &DiffError) -> RecoveryOutcome<T> {
    let code = match error.code {
        "cooperation_required" => RecoveryCode::NoCooperativeSafePoint,
        "decode" => RecoveryCode::Decode,
        "integrity" => RecoveryCode::Integrity,
        "metadata_mismatch" => RecoveryCode::MetadataMismatch,
        "missing_reference" => RecoveryCode::MissingReference,
        "non_canonical" => RecoveryCode::NonCanonical,
        "schema_version" => RecoveryCode::SchemaVersion,
        "storage" => RecoveryCode::Storage,
        _ => RecoveryCode::InvalidContext,
    };
    let issue = issue(
        code,
        format!("{} checkpoint: {}", diff_side(error.side), error.detail),
    );
    match error.kind {
        DiffErrorKind::UnsupportedSchema | DiffErrorKind::UnsupportedCheckpoint => {
            RecoveryOutcome::Unsupported(issue)
        }
        DiffErrorKind::InvalidCheckpoint => RecoveryOutcome::Failed(issue),
    }
}

const fn diff_side(side: epoch_diff::DiffSide) -> &'static str {
    match side {
        epoch_diff::DiffSide::Before => "before",
        epoch_diff::DiffSide::After => "after",
    }
}

fn issue(code: RecoveryCode, detail: String) -> RecoveryIssue {
    RecoveryIssue { code, detail }
}

fn failed<T>(code: RecoveryCode, detail: String) -> RecoveryOutcome<T> {
    RecoveryOutcome::Failed(issue(code, detail))
}

fn failed_rejection(code: RecoveryCode, detail: String) -> RecoveryRejection {
    RecoveryRejection::Failed(issue(code, detail))
}

fn unsupported_rejection(code: RecoveryCode, detail: String) -> RecoveryRejection {
    RecoveryRejection::Unsupported(issue(code, detail))
}

fn unix_ms() -> Result<i64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?;
    i64::try_from(duration.as_millis()).map_err(|_| "wall clock does not fit i64".to_owned())
}
