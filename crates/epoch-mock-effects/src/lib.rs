//! Durable idempotent mock effects for recovery and replay tests.

use std::path::Path;

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
    pub payload_hash: String,
    pub receipt_id: String,
    pub committed_at_unix_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DeliveryOutcome {
    Respond(CommittedOperation),
    WithholdResponse { operation_id: String },
}

#[derive(Debug)]
pub struct MockEffectStore;

impl MockEffectStore {
    /// Opens or creates the durable mock-service database.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be configured or migrated.
    pub fn open(_path: impl AsRef<Path>) -> Result<Self, MockEffectError> {
        Err(MockEffectError::NotImplemented)
    }

    /// Commits an operation idempotently and optionally simulates a lost response.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, storage failure, or reuse of an operation ID with a
    /// different payload.
    pub fn submit(
        &mut self,
        _request: &OperationRequest,
        _withhold_response: bool,
    ) -> Result<DeliveryOutcome, MockEffectError> {
        Err(MockEffectError::NotImplemented)
    }

    /// Looks up a previously committed operation.
    ///
    /// # Errors
    ///
    /// Returns an error when durable state cannot be read.
    pub fn lookup(
        &self,
        _operation_id: &str,
    ) -> Result<Option<CommittedOperation>, MockEffectError> {
        Err(MockEffectError::NotImplemented)
    }
}

#[derive(Debug, Error)]
pub enum MockEffectError {
    #[error("mock effect service is not implemented")]
    NotImplemented,
    #[error("operation ID {operation_id:?} was already used with different content")]
    OperationConflict { operation_id: String },
}
