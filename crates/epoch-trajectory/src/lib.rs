//! Privacy-safe trajectory export for learned, advisory runtime policies.
//!
//! The exporter intentionally excludes event payloads, blob hashes, filesystem paths, capability
//! handles, effect arguments, and raw durable identifiers. Models receive structural execution
//! metadata only. Learned outputs are advisory and are never part of Epoch's trusted authority or
//! effect-commit path.

use std::{
    collections::HashMap,
    fmt::Write as _,
    fs::OpenOptions,
    io::{BufWriter, Write as _},
    path::Path,
    str::FromStr,
};

use epoch_core::{BranchId, EpochId, SessionId};
use epoch_storage::{StorageError, Store};
use rusqlite::{OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

/// Version of the JSONL record contract consumed by the ML package.
pub const TRAJECTORY_SCHEMA_VERSION: u32 = 1;

/// Conservative default bound for one session export.
pub const DEFAULT_MAX_BRANCHES: usize = 256;

/// Conservative default bound for one branch export.
pub const DEFAULT_MAX_EVENTS_PER_BRANCH: usize = 256;

/// Export safety limits. Limits fail the whole export instead of silently truncating examples.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportLimits {
    pub max_branches: usize,
    pub max_events_per_branch: usize,
}

impl Default for ExportLimits {
    fn default() -> Self {
        Self {
            max_branches: DEFAULT_MAX_BRANCHES,
            max_events_per_branch: DEFAULT_MAX_EVENTS_PER_BRANCH,
        }
    }
}

/// Privacy contract attached to every exported trajectory.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyProfile {
    /// Structural event metadata only; user and tool payloads are absent.
    MetadataOnly,
}

/// One branch trajectory and its terminal learning label, if known.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchTrajectory {
    pub schema_version: u32,
    pub privacy_profile: PrivacyProfile,
    pub trajectory_id: String,
    pub task_group_id: String,
    pub session_group_id: String,
    pub candidate_group_id: String,
    pub branch_depth: u32,
    pub success_label: Option<bool>,
    pub value_label: Option<f64>,
    pub events: Vec<TrajectoryEvent>,
    pub summary: TrajectorySummary,
}

/// Metadata-only representation of one durable event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryEvent {
    pub position: u64,
    pub delta_monotonic_ns: u64,
    pub actor: String,
    pub kind: String,
    pub status: String,
    pub references_epoch: bool,
    pub has_causal_parent: bool,
}

/// Bounded numerical features that can feed baselines without parsing payloads.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectorySummary {
    pub event_count: u64,
    pub duration_monotonic_ns: u64,
    pub started_events: u64,
    pub succeeded_events: u64,
    pub failed_events: u64,
    pub denied_events: u64,
    pub unknown_events: u64,
}

#[derive(Clone, Debug)]
struct StoredBranch {
    id: BranchId,
    parent_id: Option<BranchId>,
    fork_epoch_id: Option<EpochId>,
    state: String,
}

/// Exports every branch in one session as deterministic, metadata-only learning records.
///
/// `task_group` is caller-supplied experiment metadata used only to derive an opaque grouping
/// digest. All sessions for the same task or repository must use the same task group so a dataset
/// splitter can keep them on one side of the train/evaluation boundary.
///
/// # Errors
///
/// Returns a typed error for invalid grouping metadata, absent/corrupt state, safety-limit
/// violations, non-contiguous events, or regressing monotonic clocks.
pub fn export_session(
    database_path: impl AsRef<Path>,
    session_id: SessionId,
    task_group: &str,
    limits: ExportLimits,
) -> Result<Vec<BranchTrajectory>, ExportError> {
    validate_task_group(task_group)?;
    validate_limits(limits)?;
    let store = Store::open(database_path)?;
    let connection = store.connection();
    let session_exists = connection
        .query_row(
            "SELECT 1 FROM sessions WHERE id = ?1",
            [session_id.to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !session_exists {
        return Err(ExportError::SessionNotFound { session_id });
    }

    let branches = read_branches(connection, session_id, limits.max_branches)?;
    let branch_map = branches
        .iter()
        .map(|branch| (branch.id, branch))
        .collect::<HashMap<_, _>>();
    let task_group_id = pseudonym(&["task", task_group]);
    let session_group_id = pseudonym(&["session", &session_id.to_string()]);
    let mut records = Vec::with_capacity(branches.len());
    for branch in &branches {
        let branch_depth = branch_depth(branch, &branch_map)?;
        let candidate_group_id = candidate_group(session_id, branch);
        let events = read_events(
            connection,
            session_id,
            branch.id,
            limits.max_events_per_branch,
        )?;
        let summary = summarize(&events)?;
        let (success_label, value_label) = learning_labels(&branch.state);
        records.push(BranchTrajectory {
            schema_version: TRAJECTORY_SCHEMA_VERSION,
            privacy_profile: PrivacyProfile::MetadataOnly,
            trajectory_id: pseudonym(&[
                "trajectory",
                &session_id.to_string(),
                &branch.id.to_string(),
            ]),
            task_group_id: task_group_id.clone(),
            session_group_id: session_group_id.clone(),
            candidate_group_id,
            branch_depth,
            success_label,
            value_label,
            events,
            summary,
        });
    }
    Ok(records)
}

fn validate_task_group(value: &str) -> Result<(), ExportError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(ExportError::InvalidTaskGroup)
    }
}

const fn validate_limits(limits: ExportLimits) -> Result<(), ExportError> {
    if limits.max_branches == 0 || limits.max_events_per_branch == 0 {
        Err(ExportError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn read_branches(
    connection: &rusqlite::Connection,
    session_id: SessionId,
    limit: usize,
) -> Result<Vec<StoredBranch>, ExportError> {
    let query_limit = limit.checked_add(1).ok_or(ExportError::InvalidLimits)?;
    let query_limit = i64::try_from(query_limit).map_err(|_| ExportError::InvalidLimits)?;
    let mut statement = connection.prepare(
        "SELECT id, parent_branch_id, fork_epoch_id, state
         FROM branches
         WHERE session_id = ?1
         ORDER BY created_at_unix_ms ASC, id ASC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![session_id.to_string(), query_limit], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let raw = rows.collect::<Result<Vec<_>, _>>()?;
    if raw.len() > limit {
        return Err(ExportError::BranchLimitExceeded { limit });
    }
    raw.into_iter()
        .map(|(id, parent_id, fork_epoch_id, state)| {
            validate_branch_state(&state)?;
            Ok(StoredBranch {
                id: parse_id("branches", "id", &id)?,
                parent_id: parent_id
                    .as_deref()
                    .map(|value| parse_id("branches", "parent_branch_id", value))
                    .transpose()?,
                fork_epoch_id: fork_epoch_id
                    .as_deref()
                    .map(|value| parse_id("branches", "fork_epoch_id", value))
                    .transpose()?,
                state,
            })
        })
        .collect()
}

fn validate_branch_state(state: &str) -> Result<(), ExportError> {
    if matches!(
        state,
        "created" | "running" | "suspended" | "completed" | "promoted" | "abandoned" | "failed"
    ) {
        Ok(())
    } else {
        Err(ExportError::InvalidStoredValue {
            table: "branches",
            field: "state",
        })
    }
}

fn parse_id<T>(table: &'static str, field: &'static str, value: &str) -> Result<T, ExportError>
where
    T: FromStr,
{
    value
        .parse()
        .map_err(|_| ExportError::InvalidStoredValue { table, field })
}

fn branch_depth(
    branch: &StoredBranch,
    branches: &HashMap<BranchId, &StoredBranch>,
) -> Result<u32, ExportError> {
    let mut depth = 0_u32;
    let mut cursor = branch;
    for _ in 0..=branches.len() {
        let Some(parent_id) = cursor.parent_id else {
            return Ok(depth);
        };
        depth = depth.checked_add(1).ok_or(ExportError::InvalidLineage)?;
        cursor = branches
            .get(&parent_id)
            .copied()
            .ok_or(ExportError::InvalidLineage)?;
    }
    Err(ExportError::InvalidLineage)
}

fn candidate_group(session_id: SessionId, branch: &StoredBranch) -> String {
    match (branch.parent_id, branch.fork_epoch_id) {
        (Some(parent), Some(epoch)) => pseudonym(&[
            "candidates",
            &session_id.to_string(),
            &parent.to_string(),
            &epoch.to_string(),
        ]),
        _ => pseudonym(&["root-candidate", &session_id.to_string()]),
    }
}

fn read_events(
    connection: &rusqlite::Connection,
    session_id: SessionId,
    branch_id: BranchId,
    limit: usize,
) -> Result<Vec<TrajectoryEvent>, ExportError> {
    let query_limit = limit.checked_add(1).ok_or(ExportError::InvalidLimits)?;
    let query_limit = i64::try_from(query_limit).map_err(|_| ExportError::InvalidLimits)?;
    let mut statement = connection.prepare(
        "SELECT sequence, monotonic_ns, actor, kind, status,
                epoch_id IS NOT NULL, causal_parent_id IS NOT NULL
         FROM events
         WHERE session_id = ?1 AND branch_id = ?2
         ORDER BY sequence ASC, id ASC
         LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![session_id.to_string(), branch_id.to_string(), query_limit],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, bool>(6)?,
            ))
        },
    )?;
    let raw = rows.collect::<Result<Vec<_>, _>>()?;
    if raw.len() > limit {
        return Err(ExportError::EventLimitExceeded { branch_id, limit });
    }
    let mut previous_monotonic_ns = None;
    let mut events = Vec::with_capacity(raw.len());
    for (expected_source_position, row) in raw.into_iter().enumerate() {
        let source_position =
            u64::try_from(row.0).map_err(|_| ExportError::InvalidStoredValue {
                table: "events",
                field: "sequence",
            })?;
        let expected_source_position =
            u64::try_from(expected_source_position).map_err(|_| ExportError::InvalidLimits)?;
        if source_position != expected_source_position {
            return Err(ExportError::NonContiguousEvents { branch_id });
        }
        let monotonic_ns = u64::try_from(row.1).map_err(|_| ExportError::InvalidStoredValue {
            table: "events",
            field: "monotonic_ns",
        })?;
        if is_terminal_outcome_kind(&row.3) {
            continue;
        }
        let delta_monotonic_ns = match previous_monotonic_ns {
            None => 0,
            Some(previous) => monotonic_ns
                .checked_sub(previous)
                .ok_or(ExportError::MonotonicClockRegression { branch_id })?,
        };
        previous_monotonic_ns = Some(monotonic_ns);
        let position = u64::try_from(events.len()).map_err(|_| ExportError::InvalidLimits)?;
        events.push(TrajectoryEvent {
            position,
            delta_monotonic_ns,
            actor: row.2,
            kind: row.3,
            status: row.4,
            references_epoch: row.5,
            has_causal_parent: row.6,
        });
    }
    Ok(events)
}

fn summarize(events: &[TrajectoryEvent]) -> Result<TrajectorySummary, ExportError> {
    let mut summary = TrajectorySummary {
        event_count: u64::try_from(events.len()).map_err(|_| ExportError::InvalidLimits)?,
        ..TrajectorySummary::default()
    };
    for event in events {
        summary.duration_monotonic_ns = summary
            .duration_monotonic_ns
            .checked_add(event.delta_monotonic_ns)
            .ok_or(ExportError::DurationOverflow)?;
        let counter = match event.status.as_str() {
            "started" => &mut summary.started_events,
            "succeeded" => &mut summary.succeeded_events,
            "failed" => &mut summary.failed_events,
            "denied" => &mut summary.denied_events,
            "unknown" => &mut summary.unknown_events,
            _ => {
                return Err(ExportError::InvalidStoredValue {
                    table: "events",
                    field: "status",
                });
            }
        };
        *counter = counter
            .checked_add(1)
            .ok_or(ExportError::DurationOverflow)?;
    }
    Ok(summary)
}

const fn learning_labels(state: &str) -> (Option<bool>, Option<f64>) {
    match state.as_bytes() {
        b"promoted" => (Some(true), Some(1.0)),
        b"completed" => (Some(true), Some(0.75)),
        b"failed" | b"abandoned" => (Some(false), Some(0.0)),
        _ => (None, None),
    }
}

fn is_terminal_outcome_kind(kind: &str) -> bool {
    matches!(
        kind,
        "agent.completion" | "process.exited" | "supervisor.failure"
    )
}

fn pseudonym(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"epoch-trajectory-v1\0");
    for part in parts {
        digest.update(part.len().to_le_bytes());
        digest.update(part.as_bytes());
    }
    digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}

/// Writes newline-delimited records to a new private file and refuses replacement.
///
/// # Errors
///
/// Returns an error when `output` already exists, is not a regular creatable file, cannot be
/// written privately, or a record cannot be serialized.
pub fn write_jsonl_new(
    output: impl AsRef<Path>,
    records: &[BranchTrajectory],
) -> Result<(), ExportError> {
    let output = output.as_ref();
    if output.exists() {
        return Err(ExportError::OutputAlreadyExists);
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options
        .open(output)
        .map_err(|source| ExportError::Output { source })?;
    let result = (|| {
        let mut writer = BufWriter::new(file);
        for record in records {
            serde_json::to_writer(&mut writer, record)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        writer.get_ref().sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

/// Typed export and persistence failures that do not include private event contents.
#[derive(Debug, Error)]
pub enum ExportError {
    #[error("invalid task group; use 1-128 lowercase ASCII letters, digits, '.', '_', or '-'")]
    InvalidTaskGroup,
    #[error("export limits must be positive and representable")]
    InvalidLimits,
    #[error("session {session_id} does not exist")]
    SessionNotFound { session_id: SessionId },
    #[error("branch limit {limit} exceeded")]
    BranchLimitExceeded { limit: usize },
    #[error("event limit {limit} exceeded for branch {branch_id}")]
    EventLimitExceeded { branch_id: BranchId, limit: usize },
    #[error("stored {table}.{field} is invalid")]
    InvalidStoredValue {
        table: &'static str,
        field: &'static str,
    },
    #[error("stored branch lineage is invalid")]
    InvalidLineage,
    #[error("stored events are not contiguous for branch {branch_id}")]
    NonContiguousEvents { branch_id: BranchId },
    #[error("monotonic clock regressed for branch {branch_id}")]
    MonotonicClockRegression { branch_id: BranchId },
    #[error("trajectory duration or counter overflowed")]
    DurationOverflow,
    #[error("output already exists; trajectory export never overwrites")]
    OutputAlreadyExists,
    #[error("trajectory output is unavailable: {source}")]
    Output { source: std::io::Error },
    #[error("trusted storage is unavailable: {0}")]
    Storage(#[from] StorageError),
    #[error("trusted query failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("trajectory serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("trajectory write failed: {0}")]
    Io(#[from] std::io::Error),
}

impl ExportError {
    /// Returns true when the caller can fix the request without repairing trusted state.
    #[must_use]
    pub const fn is_user_error(&self) -> bool {
        matches!(
            self,
            Self::InvalidTaskGroup
                | Self::InvalidLimits
                | Self::SessionNotFound { .. }
                | Self::BranchLimitExceeded { .. }
                | Self::EventLimitExceeded { .. }
                | Self::OutputAlreadyExists
                | Self::Output { .. }
                | Self::Io(_)
        )
    }
}
