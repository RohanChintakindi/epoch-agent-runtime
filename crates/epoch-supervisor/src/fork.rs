use std::{
    str::FromStr as _,
    time::{SystemTime, UNIX_EPOCH},
};

use epoch_blob::BlobHash;
use epoch_checkpoint::ResumeCursors;
use epoch_core::{BranchId, EpochId, SessionId};
use epoch_events::{EventQuery, JournalError};
use epoch_protocol::{Message, ToolOutcome, decode_line};
use epoch_storage::Store;
use rusqlite::{OptionalExtension as _, Transaction, TransactionBehavior, params};
use serde::Serialize;

use crate::{DirectSupervisor, RecoveryCode, RecoveryIssue, RecoveryOutcome};

use crate::recovery::ValidatedApplicationSource;

const MAX_BRANCH_NAME_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryOutcome {
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UnsupportedBoundary {
    pub outcome: BoundaryOutcome,
    pub code: &'static str,
    pub detail: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RecordedModelResult {
    pub protocol_sequence: u64,
    pub request_id: String,
    pub output_hash_claim: BlobHash,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RecordedToolResult {
    pub protocol_sequence: u64,
    pub call_id: String,
    pub outcome: ToolOutcome,
    pub output_hash_claim: Option<BlobHash>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RecordedReplayResults {
    pub model: Vec<RecordedModelResult>,
    pub tool: Vec<RecordedToolResult>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReplayReport {
    pub resume_cursor: ResumeCursors,
    pub recorded_results: RecordedReplayResults,
    pub continuation: UnsupportedBoundary,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EffectFrontierBoundary {
    pub outcome: BoundaryOutcome,
    pub source_epoch_frontier: u64,
    pub inherited_frontier: Option<u64>,
    pub code: &'static str,
    pub detail: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ForkBranchReport {
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub name: String,
    pub state: String,
    pub parent_branch_id: BranchId,
    pub fork_epoch_id: EpochId,
    pub fork_point_sequence: u64,
    pub application_component_hash: BlobHash,
    pub next_event_sequence: u64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub replay: ReplayReport,
    pub effect_frontier: EffectFrontierBoundary,
}

struct StoredFork {
    session_id: SessionId,
    parent_branch_id: BranchId,
    fork_epoch_id: EpochId,
    name: String,
    fork_point_sequence: u64,
    component_hash: BlobHash,
    state: String,
    next_event_sequence: u64,
    created_at_unix_ms: i64,
    updated_at_unix_ms: i64,
}

struct StoredForkRow {
    session: String,
    parent: Option<String>,
    epoch: Option<String>,
    name: Option<String>,
    point: Option<i64>,
    component: Option<String>,
    state: String,
    next: i64,
    created: i64,
    updated: i64,
}

enum StoredForkLoadError {
    Unsupported(RecoveryIssue),
    Failed(RecoveryIssue),
}

impl StoredForkLoadError {
    fn into_outcome<T>(self) -> RecoveryOutcome<T> {
        match self {
            Self::Unsupported(issue) => RecoveryOutcome::Unsupported(issue),
            Self::Failed(issue) => RecoveryOutcome::Failed(issue),
        }
    }
}

impl DirectSupervisor {
    /// Creates a durable logical child branch from one validated committed application epoch.
    #[must_use]
    pub fn fork_application_epoch(
        &self,
        epoch_id: EpochId,
        name: &str,
    ) -> RecoveryOutcome<ForkBranchReport> {
        if !valid_branch_name(name) {
            return failed(
                RecoveryCode::InvalidBranchName,
                format!(
                    "branch name must be 1-{MAX_BRANCH_NAME_BYTES} bytes of lowercase \
                     ASCII letters, digits, '.', '_' or '-', beginning with a letter or digit"
                ),
            );
        }
        let source = match self.validated_application_source(epoch_id) {
            RecoveryOutcome::Supported(source) => source,
            RecoveryOutcome::Unsupported(issue) => return RecoveryOutcome::Unsupported(issue),
            RecoveryOutcome::Failed(issue) => return RecoveryOutcome::Failed(issue),
        };
        let replay = match self.replay_report(&source) {
            Ok(replay) => replay,
            Err(issue) => return RecoveryOutcome::Failed(issue),
        };
        let branch_id = BranchId::new();
        let timestamp = match unix_ms() {
            Ok(timestamp) => timestamp,
            Err(issue) => return RecoveryOutcome::Failed(issue),
        };
        if let Err(issue) = self.persist_fork(&source, branch_id, name, timestamp) {
            return RecoveryOutcome::Failed(issue);
        }
        RecoveryOutcome::Supported(report(
            &source,
            StoredFork {
                session_id: source.session_id,
                parent_branch_id: source.branch_id,
                fork_epoch_id: source.epoch_id,
                name: name.to_owned(),
                fork_point_sequence: source.context.cursors.boundary_sequence,
                component_hash: source.component_hash.clone(),
                state: "created".to_owned(),
                next_event_sequence: 0,
                created_at_unix_ms: timestamp,
                updated_at_unix_ms: timestamp,
            },
            branch_id,
            replay,
        ))
    }

    /// Revalidates and inspects durable fork lineage after a process restart.
    #[must_use]
    pub fn inspect_fork_branch(&self, branch_id: BranchId) -> RecoveryOutcome<ForkBranchReport> {
        let stored = match self.load_stored_fork(branch_id) {
            Ok(stored) => stored,
            Err(error) => return error.into_outcome(),
        };
        let source = match self.validated_application_source(stored.fork_epoch_id) {
            RecoveryOutcome::Supported(source) => source,
            RecoveryOutcome::Unsupported(issue) => return RecoveryOutcome::Unsupported(issue),
            RecoveryOutcome::Failed(issue) => return RecoveryOutcome::Failed(issue),
        };
        if source.session_id != stored.session_id
            || source.branch_id != stored.parent_branch_id
            || source.component_hash != stored.component_hash
            || source.context.cursors.boundary_sequence != stored.fork_point_sequence
        {
            return failed(
                RecoveryCode::MetadataMismatch,
                "stored fork lineage does not match its validated source checkpoint".to_owned(),
            );
        }
        let replay = match self.replay_report(&source) {
            Ok(replay) => replay,
            Err(issue) => return RecoveryOutcome::Failed(issue),
        };
        RecoveryOutcome::Supported(report(&source, stored, branch_id, replay))
    }

    fn persist_fork(
        &self,
        source: &ValidatedApplicationSource,
        branch_id: BranchId,
        name: &str,
        timestamp: i64,
    ) -> Result<(), RecoveryIssue> {
        let fork_point = i64::try_from(source.context.cursors.boundary_sequence)
            .map_err(|error| persistence(error.to_string()))?;
        let mut store =
            Store::open(&self.database_path).map_err(|error| persistence(error.to_string()))?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence(error.to_string()))?;
        ensure_name_available(&transaction, source.session_id, name)?;
        ensure_source_unchanged(&transaction, source)?;
        transaction
            .execute(
                "INSERT INTO branches \
                 (id, session_id, parent_branch_id, fork_epoch_id, name, fork_point_sequence, \
                  fork_component_hash, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'created', ?8, ?8)",
                params![
                    branch_id.to_string(),
                    source.session_id.to_string(),
                    source.branch_id.to_string(),
                    source.epoch_id.to_string(),
                    name,
                    fork_point,
                    source.component_hash.to_string(),
                    timestamp,
                ],
            )
            .map_err(|error| persistence(error.to_string()))?;
        transaction
            .commit()
            .map_err(|error| persistence(error.to_string()))
    }

    fn load_stored_fork(&self, branch_id: BranchId) -> Result<StoredFork, StoredForkLoadError> {
        let store = Store::open(&self.database_path)
            .map_err(|error| StoredForkLoadError::Failed(persistence(error.to_string())))?;
        let row = store
            .connection()
            .query_row(
                "SELECT session_id, parent_branch_id, fork_epoch_id, name, fork_point_sequence, \
                        fork_component_hash, state, next_event_sequence, created_at_unix_ms, \
                        updated_at_unix_ms \
                 FROM branches WHERE id = ?1",
                [branch_id.to_string()],
                |row| {
                    Ok(StoredForkRow {
                        session: row.get(0)?,
                        parent: row.get(1)?,
                        epoch: row.get(2)?,
                        name: row.get(3)?,
                        point: row.get(4)?,
                        component: row.get(5)?,
                        state: row.get(6)?,
                        next: row.get(7)?,
                        created: row.get(8)?,
                        updated: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(|error| StoredForkLoadError::Failed(persistence(error.to_string())))?;
        let Some(row) = row else {
            return Err(StoredForkLoadError::Failed(RecoveryIssue {
                code: RecoveryCode::NotFound,
                detail: format!("branch {branch_id} was not found"),
            }));
        };
        let (Some(parent), Some(epoch), Some(name), Some(point), Some(component)) =
            (row.parent, row.epoch, row.name, row.point, row.component)
        else {
            return Err(StoredForkLoadError::Unsupported(RecoveryIssue {
                code: RecoveryCode::UnsupportedMode,
                detail: format!("branch {branch_id} is not a fork"),
            }));
        };
        parse_stored_fork(StoredForkRow {
            session: row.session,
            parent: Some(parent),
            epoch: Some(epoch),
            name: Some(name),
            point: Some(point),
            component: Some(component),
            state: row.state,
            next: row.next,
            created: row.created,
            updated: row.updated,
        })
        .map_err(|detail| {
            StoredForkLoadError::Failed(RecoveryIssue {
                code: RecoveryCode::MetadataMismatch,
                detail,
            })
        })
    }

    fn replay_report(
        &self,
        source: &ValidatedApplicationSource,
    ) -> Result<ReplayReport, RecoveryIssue> {
        let mut model = Vec::new();
        let mut tool = Vec::new();
        let events = self
            .journal
            .query(&EventQuery::for_session(source.session_id))
            .map_err(|error| journal_issue(&error))?;
        for event in events
            .into_iter()
            .filter(|event| event.branch_id == source.branch_id)
        {
            if !matches!(event.kind.as_str(), "model.response" | "tool.result") {
                continue;
            }
            let payload = self
                .journal
                .read_payload(&event)
                .map_err(|error| journal_issue(&error))?;
            let encoded =
                serde_json::to_vec(&payload).map_err(|error| persistence(error.to_string()))?;
            let envelope = decode_line(&encoded).map_err(|error| RecoveryIssue {
                code: RecoveryCode::MetadataMismatch,
                detail: format!("stored replay result is invalid: {error}"),
            })?;
            if envelope.sequence > source.context.cursors.boundary_sequence {
                continue;
            }
            match envelope.message {
                Message::ModelResponse(result) => model.push(RecordedModelResult {
                    protocol_sequence: envelope.sequence,
                    request_id: result.request_id,
                    output_hash_claim: result.output_hash,
                }),
                Message::ToolResult(result) => tool.push(RecordedToolResult {
                    protocol_sequence: envelope.sequence,
                    call_id: result.call_id,
                    outcome: result.outcome,
                    output_hash_claim: result.output_hash,
                }),
                _ => {
                    return Err(RecoveryIssue {
                        code: RecoveryCode::MetadataMismatch,
                        detail: "stored replay event kind does not match its payload".to_owned(),
                    });
                }
            }
        }
        model.sort_by_key(|result| result.protocol_sequence);
        tool.sort_by_key(|result| result.protocol_sequence);
        Ok(ReplayReport {
            resume_cursor: source.context.cursors.clone(),
            recorded_results: RecordedReplayResults { model, tool },
            continuation: UnsupportedBoundary {
                outcome: BoundaryOutcome::Unsupported,
                code: "agent_resume_adapter_unavailable",
                detail: "recorded result hashes are inspectable, but result bytes and an agent \
                         resume adapter are not represented by application context schema v1",
            },
        })
    }
}

fn report(
    source: &ValidatedApplicationSource,
    stored: StoredFork,
    branch_id: BranchId,
    replay: ReplayReport,
) -> ForkBranchReport {
    ForkBranchReport {
        session_id: stored.session_id,
        branch_id,
        name: stored.name,
        state: stored.state,
        parent_branch_id: stored.parent_branch_id,
        fork_epoch_id: stored.fork_epoch_id,
        fork_point_sequence: stored.fork_point_sequence,
        application_component_hash: stored.component_hash,
        next_event_sequence: stored.next_event_sequence,
        created_at_unix_ms: stored.created_at_unix_ms,
        updated_at_unix_ms: stored.updated_at_unix_ms,
        replay,
        effect_frontier: EffectFrontierBoundary {
            outcome: BoundaryOutcome::Unsupported,
            source_epoch_frontier: source.effect_frontier,
            inherited_frontier: None,
            code: "effect_frontier_not_integrated",
            detail: "effect history remains durable and non-rollbackable, but fork inheritance \
                     and frontier comparison are not integrated",
        },
    }
}

fn ensure_name_available(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    name: &str,
) -> Result<(), RecoveryIssue> {
    let exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM branches WHERE session_id = ?1 AND name = ?2)",
            params![session_id.to_string(), name],
            |row| row.get(0),
        )
        .map_err(|error| persistence(error.to_string()))?;
    if exists {
        Err(RecoveryIssue {
            code: RecoveryCode::BranchNameConflict,
            detail: format!("branch name {name:?} already exists in session {session_id}"),
        })
    } else {
        Ok(())
    }
}

fn ensure_source_unchanged(
    transaction: &Transaction<'_>,
    source: &ValidatedApplicationSource,
) -> Result<(), RecoveryIssue> {
    let boundary = i64::try_from(source.context.cursors.boundary_sequence)
        .map_err(|error| persistence(error.to_string()))?;
    let unchanged: bool = transaction
        .query_row(
            "SELECT EXISTS( \
                 SELECT 1 FROM epochs e \
                 JOIN snapshot_components c ON c.epoch_id = e.id \
                 WHERE e.id = ?1 AND e.session_id = ?2 AND e.branch_id = ?3 \
                   AND e.status = 'committed' AND c.kind = 'application_context' \
                   AND c.status = 'committed' AND c.blob_hash = ?4 \
                   AND json_extract(c.metadata_json, '$.boundary_sequence') = ?5 \
             )",
            params![
                source.epoch_id.to_string(),
                source.session_id.to_string(),
                source.branch_id.to_string(),
                source.component_hash.to_string(),
                boundary,
            ],
            |row| row.get(0),
        )
        .map_err(|error| persistence(error.to_string()))?;
    if unchanged {
        Ok(())
    } else {
        Err(RecoveryIssue {
            code: RecoveryCode::MetadataMismatch,
            detail: "source epoch changed while the fork was being created".to_owned(),
        })
    }
}

fn parse_stored_fork(row: StoredForkRow) -> Result<StoredFork, String> {
    let parent = row
        .parent
        .ok_or_else(|| "missing parent branch".to_owned())?;
    let epoch = row.epoch.ok_or_else(|| "missing fork epoch".to_owned())?;
    let name = row.name.ok_or_else(|| "missing branch name".to_owned())?;
    let point = row.point.ok_or_else(|| "missing fork point".to_owned())?;
    let component = row
        .component
        .ok_or_else(|| "missing fork component".to_owned())?;
    Ok(StoredFork {
        session_id: SessionId::from_str(&row.session).map_err(|error| error.to_string())?,
        parent_branch_id: BranchId::from_str(&parent).map_err(|error| error.to_string())?,
        fork_epoch_id: EpochId::from_str(&epoch).map_err(|error| error.to_string())?,
        name,
        fork_point_sequence: u64::try_from(point).map_err(|error| error.to_string())?,
        component_hash: BlobHash::from_str(&component).map_err(|error| error.to_string())?,
        state: row.state,
        next_event_sequence: u64::try_from(row.next).map_err(|error| error.to_string())?,
        created_at_unix_ms: row.created,
        updated_at_unix_ms: row.updated,
    })
}

fn valid_branch_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.first().is_some_and(|first| {
        bytes.len() <= MAX_BRANCH_NAME_BYTES
            && (first.is_ascii_lowercase() || first.is_ascii_digit())
            && bytes.iter().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
    })
}

fn journal_issue(error: &JournalError) -> RecoveryIssue {
    persistence(error.to_string())
}

fn unix_ms() -> Result<i64, RecoveryIssue> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| persistence(error.to_string()))?;
    i64::try_from(duration.as_millis()).map_err(|error| persistence(error.to_string()))
}

fn persistence(detail: String) -> RecoveryIssue {
    RecoveryIssue {
        code: RecoveryCode::Persistence,
        detail,
    }
}

fn failed<T>(code: RecoveryCode, detail: String) -> RecoveryOutcome<T> {
    RecoveryOutcome::Failed(RecoveryIssue { code, detail })
}
