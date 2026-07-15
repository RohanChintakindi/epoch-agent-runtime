//! Append-only event journal for trusted Epoch execution history.

use std::{ops::RangeInclusive, path::Path, str::FromStr, sync::Mutex};

use epoch_blob::{BlobError, BlobHash, BlobMetadata, BlobStore};
use epoch_core::{
    BranchId, EpochId, Event, EventActor, EventId, EventKind, EventStatus, SessionId,
};
use epoch_storage::{StorageError, Store};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde_json::Value;
use thiserror::Error;

/// Payloads larger than this encoded JSON size are stored in the content-addressed blob store.
pub const INLINE_PAYLOAD_LIMIT: usize = 16 * 1024;

/// Event fields supplied by a recorder. Identity and sequence are assigned by the journal.
#[derive(Clone, Debug)]
pub struct NewEvent {
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub epoch_id: Option<EpochId>,
    pub causal_parent: Option<EventId>,
    pub monotonic_ns: u64,
    pub occurred_at_unix_ms: i64,
    pub actor: EventActor,
    pub kind: EventKind,
    pub input_hash: Option<BlobHash>,
    pub output_hash: Option<BlobHash>,
    pub status: EventStatus,
    pub payload: Value,
}

/// Session-scoped event query with optional branch, kind, and inclusive sequence filters.
#[derive(Clone, Debug)]
pub struct EventQuery {
    pub session_id: SessionId,
    pub branch_id: Option<BranchId>,
    pub kind: Option<EventKind>,
    pub sequence: Option<RangeInclusive<u64>>,
}

impl EventQuery {
    #[must_use]
    pub const fn for_session(session_id: SessionId) -> Self {
        Self {
            session_id,
            branch_id: None,
            kind: None,
            sequence: None,
        }
    }
}

/// Durable event recorder backed by trusted `SQLite` metadata and content-addressed blobs.
#[derive(Debug)]
pub struct EventJournal {
    store: Mutex<Store>,
    blobs: BlobStore,
}

impl EventJournal {
    /// Opens the journal and its blob store.
    ///
    /// # Errors
    ///
    /// Returns an error when storage cannot be opened or migrated.
    pub fn open(
        database_path: impl AsRef<Path>,
        blob_root: impl AsRef<Path>,
    ) -> Result<Self, JournalError> {
        Ok(Self {
            store: Mutex::new(Store::open(database_path)?),
            blobs: BlobStore::open(blob_root)?,
        })
    }

    /// Atomically appends an event and allocates its per-branch sequence.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid references, storage failures, or journal corruption.
    pub fn append(&self, event: NewEvent) -> Result<Event, JournalError> {
        self.append_inner(event, AppendFault::None)
    }

    fn append_inner(&self, event: NewEvent, fault: AppendFault) -> Result<Event, JournalError> {
        if event.occurred_at_unix_ms < 0 {
            return Err(JournalError::InvalidWallTime {
                value: event.occurred_at_unix_ms,
            });
        }
        let monotonic_ns = to_sql_integer("monotonic_ns", event.monotonic_ns)?;
        if let Some(hash) = &event.input_hash {
            verify_referenced_blob(&self.blobs, "input", hash)?;
        }
        if let Some(hash) = &event.output_hash {
            verify_referenced_blob(&self.blobs, "output", hash)?;
        }

        let payload = prepare_payload(&self.blobs, &event.payload)?;

        let mut store = self.store.lock().map_err(|_| JournalError::LockPoisoned)?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_append_references(&transaction, &event, payload.external.as_ref())?;
        let sequence = allocate_sequence(&transaction, event.session_id, event.branch_id)?;

        if fault == AppendFault::AfterSequenceAllocation {
            return Err(JournalError::FaultInjected {
                point: "event.after_sequence_allocation",
            });
        }

        let recorded = insert_event(&transaction, event, sequence, monotonic_ns, payload)?;
        transaction.commit()?;
        Ok(recorded)
    }

    /// Returns events in deterministic branch/sequence/id order.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid scope, ranges, stored data, or storage failures.
    pub fn query(&self, query: &EventQuery) -> Result<Vec<Event>, JournalError> {
        let (sequence_start, sequence_end) = match &query.sequence {
            Some(range) => {
                let start = *range.start();
                let end = *range.end();
                if start > end {
                    return Err(JournalError::InvalidSequenceRange { start, end });
                }
                (
                    to_sql_integer("sequence_start", start)?,
                    to_sql_integer("sequence_end", end)?,
                )
            }
            None => (0, i64::MAX),
        };

        let store = self.store.lock().map_err(|_| JournalError::LockPoisoned)?;
        ensure_session_exists(store.connection(), query.session_id)?;
        if let Some(branch_id) = query.branch_id {
            ensure_branch_scope(store.connection(), query.session_id, branch_id)?;
        }

        let session_id = query.session_id.to_string();
        let branch_id = query.branch_id.map(|id| id.to_string());
        let kind = query.kind.as_ref().map(EventKind::as_str);
        let mut statement = store.connection().prepare(
            "SELECT id, sequence, session_id, branch_id, epoch_id, causal_parent_id, \
                    monotonic_ns, occurred_at_unix_ms, actor, kind, input_hash, output_hash, \
                    status, payload_json, payload_blob_hash \
             FROM events \
             WHERE session_id = ?1 \
               AND (?2 IS NULL OR branch_id = ?2) \
               AND (?3 IS NULL OR kind = ?3) \
               AND sequence BETWEEN ?4 AND ?5 \
             ORDER BY branch_id ASC, sequence ASC, id ASC",
        )?;
        let rows = statement.query_map(
            params![session_id, branch_id, kind, sequence_start, sequence_end],
            StoredEvent::read,
        )?;
        rows.map(|row| decode_event(row?)).collect()
    }

    /// Loads and parses either an inline or externalized payload, verifying blobs on read.
    ///
    /// # Errors
    ///
    /// Returns an error for corrupt JSON, a missing blob, or an integrity mismatch.
    pub fn read_payload(&self, event: &Event) -> Result<Value, JournalError> {
        if let Some(hash) = &event.payload_blob_hash {
            let hash = BlobHash::from_str(hash).map_err(|_| JournalError::InvalidStoredValue {
                field: "payload_blob_hash",
                value: hash.clone(),
            })?;
            let bytes = self.blobs.read(&hash)?;
            Ok(serde_json::from_slice(&bytes)?)
        } else {
            Ok(serde_json::from_str(&event.payload_json)?)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AppendFault {
    None,
    AfterSequenceAllocation,
}

#[derive(Debug)]
struct PreparedPayload {
    inline_json: String,
    external: Option<BlobMetadata>,
}

fn prepare_payload(blobs: &BlobStore, payload: &Value) -> Result<PreparedPayload, JournalError> {
    let encoded = serde_json::to_string(payload)?;
    if encoded.len() > INLINE_PAYLOAD_LIMIT {
        let external = blobs.put(encoded.as_bytes(), "application/json")?;
        Ok(PreparedPayload {
            inline_json: "{}".to_owned(),
            external: Some(external),
        })
    } else {
        Ok(PreparedPayload {
            inline_json: encoded,
            external: None,
        })
    }
}

fn validate_append_references(
    transaction: &Transaction<'_>,
    event: &NewEvent,
    external_payload: Option<&BlobMetadata>,
) -> Result<(), JournalError> {
    ensure_branch_scope(transaction, event.session_id, event.branch_id)?;
    if let Some(parent) = event.causal_parent {
        ensure_causal_parent_scope(transaction, parent, event.session_id, event.branch_id)?;
    }
    if let Some(epoch) = event.epoch_id {
        ensure_epoch_scope(transaction, epoch, event.session_id, event.branch_id)?;
    }
    if let Some(hash) = &event.input_hash {
        ensure_blob_reference(transaction, "input", hash)?;
    }
    if let Some(hash) = &event.output_hash {
        ensure_blob_reference(transaction, "output", hash)?;
    }
    if let Some(metadata) = external_payload {
        register_blob(transaction, metadata, event.occurred_at_unix_ms)?;
    }
    Ok(())
}

fn allocate_sequence(
    transaction: &Transaction<'_>,
    session_id: SessionId,
    branch_id: BranchId,
) -> Result<u64, JournalError> {
    // Repair a stale counter before allocation. This makes trusted imports and databases created
    // by an older build safe to append to.
    transaction.execute(
        "UPDATE branches \
         SET next_event_sequence = MAX( \
             next_event_sequence, \
             COALESCE((SELECT MAX(sequence) + 1 FROM events WHERE branch_id = ?1), 0) \
         ) \
         WHERE id = ?1 AND session_id = ?2",
        params![branch_id.to_string(), session_id.to_string()],
    )?;
    let sequence: i64 = transaction.query_row(
        "UPDATE branches \
         SET next_event_sequence = next_event_sequence + 1 \
         WHERE id = ?1 AND session_id = ?2 \
         RETURNING next_event_sequence - 1",
        params![branch_id.to_string(), session_id.to_string()],
        |row| row.get(0),
    )?;
    u64::try_from(sequence).map_err(|_| JournalError::InvalidStoredValue {
        field: "sequence",
        value: sequence.to_string(),
    })
}

fn insert_event(
    transaction: &Transaction<'_>,
    event: NewEvent,
    sequence: u64,
    monotonic_ns: i64,
    payload: PreparedPayload,
) -> Result<Event, JournalError> {
    let event_id = EventId::new();
    let payload_blob_hash = payload
        .external
        .as_ref()
        .map(|metadata| metadata.hash.to_string());
    transaction.execute(
        "INSERT INTO events ( \
             id, session_id, branch_id, sequence, epoch_id, causal_parent_id, monotonic_ns, \
             occurred_at_unix_ms, actor, kind, input_hash, output_hash, status, payload_json, \
             payload_blob_hash \
         ) VALUES ( \
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15 \
         )",
        params![
            event_id.to_string(),
            event.session_id.to_string(),
            event.branch_id.to_string(),
            to_sql_integer("sequence", sequence)?,
            event.epoch_id.map(|id| id.to_string()),
            event.causal_parent.map(|id| id.to_string()),
            monotonic_ns,
            event.occurred_at_unix_ms,
            actor_name(event.actor),
            event.kind.as_str(),
            event.input_hash.as_ref().map(ToString::to_string),
            event.output_hash.as_ref().map(ToString::to_string),
            status_name(event.status),
            payload.inline_json,
            payload_blob_hash,
        ],
    )?;
    Ok(Event {
        event_id,
        sequence,
        session_id: event.session_id,
        branch_id: event.branch_id,
        epoch_id: event.epoch_id,
        causal_parent: event.causal_parent,
        monotonic_ns: event.monotonic_ns,
        occurred_at_unix_ms: event.occurred_at_unix_ms,
        actor: event.actor,
        kind: event.kind,
        input_hash: event.input_hash.map(|hash| hash.to_string()),
        output_hash: event.output_hash.map(|hash| hash.to_string()),
        status: event.status,
        payload_json: payload.inline_json,
        payload_blob_hash,
    })
}

#[derive(Debug)]
struct StoredEvent {
    id: String,
    sequence: i64,
    session_id: String,
    branch_id: String,
    epoch_id: Option<String>,
    causal_parent_id: Option<String>,
    monotonic_ns: i64,
    occurred_at_unix_ms: i64,
    actor: String,
    kind: String,
    input_hash: Option<String>,
    output_hash: Option<String>,
    status: String,
    payload_json: String,
    payload_blob_hash: Option<String>,
}

impl StoredEvent {
    fn read(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            sequence: row.get(1)?,
            session_id: row.get(2)?,
            branch_id: row.get(3)?,
            epoch_id: row.get(4)?,
            causal_parent_id: row.get(5)?,
            monotonic_ns: row.get(6)?,
            occurred_at_unix_ms: row.get(7)?,
            actor: row.get(8)?,
            kind: row.get(9)?,
            input_hash: row.get(10)?,
            output_hash: row.get(11)?,
            status: row.get(12)?,
            payload_json: row.get(13)?,
            payload_blob_hash: row.get(14)?,
        })
    }
}

fn decode_event(stored: StoredEvent) -> Result<Event, JournalError> {
    let sequence =
        u64::try_from(stored.sequence).map_err(|_| JournalError::InvalidStoredValue {
            field: "sequence",
            value: stored.sequence.to_string(),
        })?;
    let monotonic_ns =
        u64::try_from(stored.monotonic_ns).map_err(|_| JournalError::InvalidStoredValue {
            field: "monotonic_ns",
            value: stored.monotonic_ns.to_string(),
        })?;
    Ok(Event {
        event_id: parse_stored_id("id", &stored.id)?,
        sequence,
        session_id: parse_stored_id("session_id", &stored.session_id)?,
        branch_id: parse_stored_id("branch_id", &stored.branch_id)?,
        epoch_id: stored
            .epoch_id
            .as_deref()
            .map(|value| parse_stored_id("epoch_id", value))
            .transpose()?,
        causal_parent: stored
            .causal_parent_id
            .as_deref()
            .map(|value| parse_stored_id("causal_parent_id", value))
            .transpose()?,
        monotonic_ns,
        occurred_at_unix_ms: stored.occurred_at_unix_ms,
        actor: parse_actor(&stored.actor)?,
        kind: EventKind::new(stored.kind.clone()).map_err(|_| {
            JournalError::InvalidStoredValue {
                field: "kind",
                value: stored.kind,
            }
        })?,
        input_hash: stored.input_hash,
        output_hash: stored.output_hash,
        status: parse_status(&stored.status)?,
        payload_json: stored.payload_json,
        payload_blob_hash: stored.payload_blob_hash,
    })
}

fn parse_stored_id<Id>(field: &'static str, value: &str) -> Result<Id, JournalError>
where
    Id: FromStr,
{
    value.parse().map_err(|_| JournalError::InvalidStoredValue {
        field,
        value: value.to_owned(),
    })
}

fn ensure_session_exists(
    connection: &Connection,
    session_id: SessionId,
) -> Result<(), JournalError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
        [session_id.to_string()],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(JournalError::SessionNotFound { session_id })
    }
}

fn ensure_branch_scope(
    connection: &Connection,
    session_id: SessionId,
    branch_id: BranchId,
) -> Result<(), JournalError> {
    let actual = connection
        .query_row(
            "SELECT session_id FROM branches WHERE id = ?1",
            [branch_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match actual {
        None => Err(JournalError::BranchNotFound { branch_id }),
        Some(actual) if actual == session_id.to_string() => Ok(()),
        Some(actual) => Err(JournalError::BranchSessionMismatch {
            branch_id,
            expected: session_id,
            actual: parse_stored_id("branches.session_id", &actual)?,
        }),
    }
}

fn ensure_causal_parent_scope(
    connection: &Connection,
    event_id: EventId,
    session_id: SessionId,
    branch_id: BranchId,
) -> Result<(), JournalError> {
    let scope = connection
        .query_row(
            "SELECT session_id, branch_id FROM events WHERE id = ?1",
            [event_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    match scope {
        None => Err(JournalError::CausalParentNotFound { event_id }),
        Some((session, branch))
            if session == session_id.to_string() && branch == branch_id.to_string() =>
        {
            Ok(())
        }
        Some(_) => Err(JournalError::CausalParentScopeMismatch { event_id }),
    }
}

fn ensure_epoch_scope(
    connection: &Connection,
    epoch_id: EpochId,
    session_id: SessionId,
    branch_id: BranchId,
) -> Result<(), JournalError> {
    let scope = connection
        .query_row(
            "SELECT session_id, branch_id FROM epochs WHERE id = ?1",
            [epoch_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    match scope {
        None => Err(JournalError::EpochNotFound { epoch_id }),
        Some((session, branch))
            if session == session_id.to_string() && branch == branch_id.to_string() =>
        {
            Ok(())
        }
        Some(_) => Err(JournalError::EpochScopeMismatch { epoch_id }),
    }
}

fn ensure_blob_reference(
    connection: &Connection,
    role: &'static str,
    hash: &BlobHash,
) -> Result<(), JournalError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM blobs WHERE hash = ?1)",
        [hash.as_str()],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(JournalError::ReferencedBlobNotFound {
            role,
            hash: hash.clone(),
        })
    }
}

fn verify_referenced_blob(
    blobs: &BlobStore,
    role: &'static str,
    hash: &BlobHash,
) -> Result<(), JournalError> {
    match blobs.read(hash) {
        Ok(_) => Ok(()),
        Err(BlobError::NotFound(_)) => Err(JournalError::ReferencedBlobNotFound {
            role,
            hash: hash.clone(),
        }),
        Err(error) => Err(error.into()),
    }
}

fn register_blob(
    transaction: &Transaction<'_>,
    metadata: &BlobMetadata,
    created_at_unix_ms: i64,
) -> Result<(), JournalError> {
    let byte_length =
        i64::try_from(metadata.length).map_err(|_| JournalError::NumericOutOfRange {
            field: "blob.byte_length",
            value: metadata.length,
        })?;
    transaction.execute(
        "INSERT INTO blobs (hash, byte_length, media_type, created_at_unix_ms) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(hash) DO NOTHING",
        params![
            metadata.hash.as_str(),
            byte_length,
            metadata.media_type,
            created_at_unix_ms
        ],
    )?;
    let recorded: (i64, String) = transaction.query_row(
        "SELECT byte_length, media_type FROM blobs WHERE hash = ?1",
        [metadata.hash.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if recorded == (byte_length, metadata.media_type.clone()) {
        Ok(())
    } else {
        Err(JournalError::BlobMetadataMismatch {
            hash: metadata.hash.clone(),
        })
    }
}

const fn actor_name(actor: EventActor) -> &'static str {
    match actor {
        EventActor::Agent => "agent",
        EventActor::Supervisor => "supervisor",
        EventActor::Tool => "tool",
        EventActor::Gateway => "gateway",
        EventActor::Operator => "operator",
    }
}

fn parse_actor(value: &str) -> Result<EventActor, JournalError> {
    match value {
        "agent" => Ok(EventActor::Agent),
        "supervisor" => Ok(EventActor::Supervisor),
        "tool" => Ok(EventActor::Tool),
        "gateway" => Ok(EventActor::Gateway),
        "operator" => Ok(EventActor::Operator),
        _ => Err(JournalError::InvalidStoredValue {
            field: "actor",
            value: value.to_owned(),
        }),
    }
}

const fn status_name(status: EventStatus) -> &'static str {
    match status {
        EventStatus::Started => "started",
        EventStatus::Succeeded => "succeeded",
        EventStatus::Failed => "failed",
        EventStatus::Denied => "denied",
        EventStatus::Unknown => "unknown",
    }
}

fn parse_status(value: &str) -> Result<EventStatus, JournalError> {
    match value {
        "started" => Ok(EventStatus::Started),
        "succeeded" => Ok(EventStatus::Succeeded),
        "failed" => Ok(EventStatus::Failed),
        "denied" => Ok(EventStatus::Denied),
        "unknown" => Ok(EventStatus::Unknown),
        _ => Err(JournalError::InvalidStoredValue {
            field: "status",
            value: value.to_owned(),
        }),
    }
}

fn to_sql_integer(field: &'static str, value: u64) -> Result<i64, JournalError> {
    i64::try_from(value).map_err(|_| JournalError::NumericOutOfRange { field, value })
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("event journal lock is poisoned")]
    LockPoisoned,
    #[error("session {session_id} does not exist")]
    SessionNotFound { session_id: SessionId },
    #[error("branch {branch_id} does not exist")]
    BranchNotFound { branch_id: BranchId },
    #[error("branch {branch_id} belongs to session {actual}, not {expected}")]
    BranchSessionMismatch {
        branch_id: BranchId,
        expected: SessionId,
        actual: SessionId,
    },
    #[error("causal parent {event_id} does not exist")]
    CausalParentNotFound { event_id: EventId },
    #[error("causal parent {event_id} is outside the target session or branch")]
    CausalParentScopeMismatch { event_id: EventId },
    #[error("epoch {epoch_id} does not exist")]
    EpochNotFound { epoch_id: EpochId },
    #[error("epoch {epoch_id} is outside the target session or branch")]
    EpochScopeMismatch { epoch_id: EpochId },
    #[error("referenced {role} blob {hash} does not exist")]
    ReferencedBlobNotFound { role: &'static str, hash: BlobHash },
    #[error("blob metadata for {hash} conflicts with its existing database record")]
    BlobMetadataMismatch { hash: BlobHash },
    #[error("invalid inclusive sequence range {start}..={end}")]
    InvalidSequenceRange { start: u64, end: u64 },
    #[error("{field} value {value} cannot be represented by SQLite INTEGER")]
    NumericOutOfRange { field: &'static str, value: u64 },
    #[error("wall time must be nonnegative, got {value}")]
    InvalidWallTime { value: i64 },
    #[error("stored event field {field} has invalid value {value:?}")]
    InvalidStoredValue { field: &'static str, value: String },
    #[error("fault injected at {point}")]
    FaultInjected { point: &'static str },
    #[error(transparent)]
    Blob(#[from] BlobError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn failure_after_sequence_allocation_rolls_back_counter_and_event() {
        let directory = TempDir::new().expect("create test runtime");
        let database = directory.path().join("state.db");
        let blobs = directory.path().join("blobs");
        let session = SessionId::new();
        let branch = BranchId::new();
        let store = Store::open(&database).expect("open database");
        store
            .connection()
            .execute(
                "INSERT INTO sessions (id, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, 'running', 0, 0)",
                [session.to_string()],
            )
            .expect("insert session");
        store
            .connection()
            .execute(
                "INSERT INTO branches \
                 (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, ?2, 'running', 0, 0)",
                params![branch.to_string(), session.to_string()],
            )
            .expect("insert branch");
        drop(store);

        let journal = EventJournal::open(&database, blobs).expect("open journal");
        let event = NewEvent {
            session_id: session,
            branch_id: branch,
            epoch_id: None,
            causal_parent: None,
            monotonic_ns: 1,
            occurred_at_unix_ms: 1,
            actor: EventActor::Supervisor,
            kind: EventKind::new("fault.test").expect("valid kind"),
            input_hash: None,
            output_hash: None,
            status: EventStatus::Failed,
            payload: json!({}),
        };
        assert!(matches!(
            journal.append_inner(event.clone(), AppendFault::AfterSequenceAllocation),
            Err(JournalError::FaultInjected { .. })
        ));
        let appended = journal.append(event).expect("append after fault");
        assert_eq!(appended.sequence, 0);

        let store = Store::open(database).expect("reopen database");
        let state: (i64, i64) = store
            .connection()
            .query_row(
                "SELECT next_event_sequence, \
                        (SELECT COUNT(*) FROM events WHERE branch_id = ?1) \
                 FROM branches WHERE id = ?1",
                [branch.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read post-fault state");
        assert_eq!(state, (1, 1));
    }
}
