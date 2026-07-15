//! Durable idempotent mock effects for recovery and replay tests.

use std::{path::Path, time::Duration};

use epoch_blob::BlobHash;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Email,
    Payment,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct OperationRequest {
    pub operation_id: String,
    pub kind: EffectKind,
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CommittedOperation {
    pub operation_id: String,
    pub kind: EffectKind,
    pub payload_hash: BlobHash,
    pub receipt_id: String,
    pub committed_at_unix_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeliveryOutcome {
    Respond(CommittedOperation),
    WithholdResponse { operation_id: String },
}

#[derive(Debug)]
pub struct MockEffectStore {
    connection: Connection,
}

impl MockEffectStore {
    /// Opens or creates the durable mock-service database.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be configured or migrated.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MockEffectError> {
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(10))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS committed_operations (
                operation_id TEXT PRIMARY KEY CHECK (length(operation_id) BETWEEN 1 AND 255),
                kind TEXT NOT NULL CHECK (kind IN ('email', 'payment')),
                payload_hash TEXT NOT NULL CHECK (
                    length(payload_hash) = 64 AND payload_hash NOT GLOB '*[^0-9a-f]*'
                ),
                payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
                receipt_id TEXT NOT NULL UNIQUE CHECK (
                    length(receipt_id) = 64 AND receipt_id NOT GLOB '*[^0-9a-f]*'
                ),
                committed_at_unix_ms INTEGER NOT NULL CHECK (committed_at_unix_ms >= 0)
            ) STRICT;",
        )?;
        Ok(Self { connection })
    }

    /// Commits an operation idempotently and optionally simulates a lost response.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, storage failure, or reuse of an operation ID with a
    /// different payload.
    pub fn submit(
        &mut self,
        request: &OperationRequest,
        withhold_response: bool,
    ) -> Result<DeliveryOutcome, MockEffectError> {
        validate_request(request)?;
        let canonical_payload = serde_json::to_vec(&(request.kind, &request.payload))?;
        let payload_hash = BlobHash::digest(&canonical_payload);

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let operation = if let Some(existing) = lookup(&transaction, &request.operation_id)? {
            if existing.kind != request.kind || existing.payload_hash != payload_hash {
                return Err(MockEffectError::OperationConflict {
                    operation_id: request.operation_id.clone(),
                });
            }
            existing
        } else {
            let receipt_id = receipt_id(&request.operation_id, &payload_hash);
            transaction.execute(
                "INSERT INTO committed_operations (
                    operation_id, kind, payload_hash, payload_json, receipt_id,
                    committed_at_unix_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, unixepoch('subsec') * 1000)",
                params![
                    request.operation_id,
                    request.kind.as_str(),
                    payload_hash.as_str(),
                    serde_json::to_string(&request.payload)?,
                    receipt_id,
                ],
            )?;
            lookup(&transaction, &request.operation_id)?.ok_or_else(|| {
                MockEffectError::CorruptStore("inserted operation was not readable".to_owned())
            })?
        };
        transaction.commit()?;

        if withhold_response {
            Ok(DeliveryOutcome::WithholdResponse {
                operation_id: request.operation_id.clone(),
            })
        } else {
            Ok(DeliveryOutcome::Respond(operation))
        }
    }

    /// Looks up a previously committed operation.
    ///
    /// # Errors
    ///
    /// Returns an error when durable state cannot be read.
    pub fn lookup(
        &self,
        operation_id: &str,
    ) -> Result<Option<CommittedOperation>, MockEffectError> {
        validate_operation_id(operation_id)?;
        lookup(&self.connection, operation_id)
    }
}

impl EffectKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Email => "email",
            Self::Payment => "payment",
        }
    }

    fn parse(value: &str) -> Result<Self, MockEffectError> {
        match value {
            "email" => Ok(Self::Email),
            "payment" => Ok(Self::Payment),
            _ => Err(MockEffectError::CorruptStore(format!(
                "unknown effect kind {value:?}"
            ))),
        }
    }
}

fn validate_request(request: &OperationRequest) -> Result<(), MockEffectError> {
    validate_operation_id(&request.operation_id)?;
    if request.payload.is_object() {
        Ok(())
    } else {
        Err(MockEffectError::InvalidRequest(
            "payload must be a JSON object".to_owned(),
        ))
    }
}

fn validate_operation_id(operation_id: &str) -> Result<(), MockEffectError> {
    let valid = !operation_id.is_empty()
        && operation_id.len() <= 255
        && operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if valid {
        Ok(())
    } else {
        Err(MockEffectError::InvalidRequest(
            "operation_id must be 1-255 ASCII letters, digits, '-', '_', '.', or ':'".to_owned(),
        ))
    }
}

fn receipt_id(operation_id: &str, payload_hash: &BlobHash) -> String {
    let mut receipt_material = Vec::with_capacity(operation_id.len() + 1 + 64);
    receipt_material.extend_from_slice(operation_id.as_bytes());
    receipt_material.push(0);
    receipt_material.extend_from_slice(payload_hash.as_str().as_bytes());
    BlobHash::digest(&receipt_material).to_string()
}

fn lookup(
    connection: &Connection,
    operation_id: &str,
) -> Result<Option<CommittedOperation>, MockEffectError> {
    let row = connection
        .query_row(
            "SELECT operation_id, kind, payload_hash, receipt_id, committed_at_unix_ms
             FROM committed_operations WHERE operation_id = ?1",
            [operation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()?;

    row.map(
        |(operation_id, kind, payload_hash, receipt_id, committed_at_unix_ms)| {
            Ok(CommittedOperation {
                operation_id,
                kind: EffectKind::parse(&kind)?,
                payload_hash: payload_hash
                    .parse::<BlobHash>()
                    .map_err(|error| MockEffectError::CorruptStore(error.to_string()))?,
                receipt_id,
                committed_at_unix_ms,
            })
        },
    )
    .transpose()
}

#[derive(Debug, Error)]
pub enum MockEffectError {
    #[error("invalid mock effect request: {0}")]
    InvalidRequest(String),
    #[error("operation ID {operation_id:?} was already used with different content")]
    OperationConflict { operation_id: String },
    #[error("mock effect database is inconsistent: {0}")]
    CorruptStore(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}
