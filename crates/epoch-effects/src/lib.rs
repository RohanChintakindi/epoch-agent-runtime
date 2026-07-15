//! Durable, fail-closed external-effect gateway.
//!
//! This crate deliberately does not implement capabilities. Callers must provide an explicit
//! [`Authorizer`], and production integrations should bind that interface to current trusted
//! authority outside the sandbox rollback domain.

use std::{
    collections::HashMap,
    fmt,
    path::Path,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use epoch_blob::{BlobError, BlobHash, BlobMetadata, BlobStore, InvalidBlobHash};
use epoch_core::{BranchId, EffectId, SessionId};
use epoch_storage::{StorageError, Store};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

const CANONICAL_SCHEMA_VERSION: u64 = 1;
const MAX_REPLAY_KEY_LENGTH: usize = 255;
const MAX_ACTION_LENGTH: usize = 255;
const MAX_RESOURCE_LENGTH: usize = 2_048;

/// Stable operation identity derived from session, branch, and replay position.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct OperationId(String);

impl OperationId {
    fn derive(session_id: SessionId, branch_id: BranchId, replay_key: &str) -> Self {
        let mut hasher = Sha256::new();
        for component in [
            b"epoch.effect.operation.v1".as_slice(),
            session_id.to_string().as_bytes(),
            branch_id.to_string().as_bytes(),
            replay_key.as_bytes(),
        ] {
            hasher.update(
                u64::try_from(component.len())
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
            hasher.update(component);
        }
        let digest = hasher.finalize();
        let mut encoded = String::with_capacity(3 + digest.len() * 2);
        encoded.push_str("op_");
        for byte in digest {
            use fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Self(encoded)
    }

    /// Returns the persistent operation identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Canonical structured request accepted by the trusted gateway.
#[derive(Clone, Debug)]
pub struct CanonicalIntent {
    session_id: SessionId,
    branch_id: BranchId,
    replay_key: String,
    action: String,
    resource: String,
    policy_revision: u64,
    operation_id: OperationId,
    canonical_bytes: Vec<u8>,
    input_hash: BlobHash,
}

impl CanonicalIntent {
    /// Validates and canonicalizes an effect request.
    ///
    /// Provider credential fields are rejected before bytes are persisted or exposed to another
    /// component. Hashes are always computed here from trusted raw bytes.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bounds, credential-bearing fields, or serialization failure.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        branch_id: BranchId,
        replay_key: impl Into<String>,
        action: impl Into<String>,
        resource: impl Into<String>,
        arguments: Value,
        policy_revision: u64,
    ) -> Result<Self, GatewayError> {
        let replay_key = replay_key.into();
        let action = action.into();
        let resource = resource.into();
        validate_text("replay_key", &replay_key, MAX_REPLAY_KEY_LENGTH)?;
        validate_text("action", &action, MAX_ACTION_LENGTH)?;
        validate_text("resource", &resource, MAX_RESOURCE_LENGTH)?;
        reject_sensitive_text(&action, "action")?;
        reject_sensitive_text(&resource, "resource")?;
        reject_sensitive_fields(&arguments, "arguments")?;
        let arguments = canonicalize_value(arguments);

        let canonical = json!({
            "action": action,
            "arguments": arguments,
            "resource": resource,
            "schema_version": CANONICAL_SCHEMA_VERSION,
        });
        let canonical_bytes = serde_json::to_vec(&canonical)?;
        let input_hash = BlobHash::digest(&canonical_bytes);
        let operation_id = OperationId::derive(session_id, branch_id, &replay_key);

        Ok(Self {
            session_id,
            branch_id,
            replay_key,
            action,
            resource,
            policy_revision,
            operation_id,
            canonical_bytes,
            input_hash,
        })
    }

    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    #[must_use]
    pub const fn branch_id(&self) -> BranchId {
        self.branch_id
    }

    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        &self.operation_id
    }

    #[must_use]
    pub fn input_hash(&self) -> &BlobHash {
        &self.input_hash
    }

    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

/// Minimal authorization context. It intentionally contains no provider credential.
#[derive(Clone, Copy, Debug)]
pub struct AuthorizationRequest<'a> {
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub operation_id: &'a OperationId,
    pub action: &'a str,
    pub resource: &'a str,
    pub input_hash: &'a BlobHash,
    pub policy_revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationDecision {
    Allow,
    Deny,
}

/// Pluggable authority decision. Implementations must consult current trusted state.
pub trait Authorizer: Send + Sync {
    fn authorize(&self, request: &AuthorizationRequest<'_>) -> AuthorizationDecision;
}

/// Explicit fail-closed authorizer useful as a safe default at composition boundaries.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAllAuthorizer;

impl Authorizer for DenyAllAuthorizer {
    fn authorize(&self, _request: &AuthorizationRequest<'_>) -> AuthorizationDecision {
        AuthorizationDecision::Deny
    }
}

/// Dispatch data provided to a trusted provider adapter.
#[derive(Clone, Copy, Debug)]
pub struct DispatchRequest<'a> {
    operation_id: &'a OperationId,
    action: &'a str,
    resource: &'a str,
    canonical_input: &'a [u8],
    input_hash: &'a BlobHash,
}

impl DispatchRequest<'_> {
    #[must_use]
    pub const fn operation_id(&self) -> &OperationId {
        self.operation_id
    }

    #[must_use]
    pub const fn action(&self) -> &str {
        self.action
    }

    #[must_use]
    pub const fn resource(&self) -> &str {
        self.resource
    }

    #[must_use]
    pub const fn canonical_input(&self) -> &[u8] {
        self.canonical_input
    }

    #[must_use]
    pub const fn input_hash(&self) -> &BlobHash {
        self.input_hash
    }
}

/// Raw trusted response returned by a provider adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchResult {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub downstream_reference: Option<String>,
}

/// Bounded failure categories prevent provider errors or credentials from entering the journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchFailureCode {
    Rejected,
    Unavailable,
    InvalidResponse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DispatchOutcome {
    Committed(DispatchResult),
    Failed(DispatchFailureCode),
}

/// Trusted adapter for an idempotent downstream operation.
pub trait EffectDispatcher: Send + Sync {
    fn dispatch(&self, request: &DispatchRequest<'_>) -> DispatchOutcome;
}

/// Deterministic in-process idempotent downstream fixture for tests and demonstrations.
///
/// This is not a production provider adapter. Its committed results are keyed by the stable
/// operation ID so even a direct duplicate dispatch returns identical raw bytes.
#[derive(Debug, Default)]
pub struct DeterministicLocalDispatcher {
    dispatches: AtomicUsize,
    committed: Mutex<HashMap<String, Vec<u8>>>,
}

impl DeterministicLocalDispatcher {
    #[must_use]
    pub fn dispatch_count(&self) -> usize {
        self.dispatches.load(Ordering::SeqCst)
    }
}

impl EffectDispatcher for DeterministicLocalDispatcher {
    fn dispatch(&self, request: &DispatchRequest<'_>) -> DispatchOutcome {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        let mut committed = self
            .committed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = committed
            .entry(request.operation_id.to_string())
            .or_insert_with(|| {
                format!(
                    r#"{{"accepted":true,"input_hash":"{}","operation_id":"{}"}}"#,
                    request.input_hash, request.operation_id
                )
                .into_bytes()
            })
            .clone();
        DispatchOutcome::Committed(DispatchResult {
            bytes: result,
            media_type: "application/json".to_owned(),
            downstream_reference: Some(format!("local:demo:{}", request.operation_id)),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectState {
    Requested,
    Prepared,
    Dispatched,
    Committed,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptState {
    Started,
    Committed,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultPoint {
    None,
    AfterPrepared,
    AfterDispatchedBeforeInvoke,
    AfterInvokeBeforeCommit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultSafety {
    KnownNotSent,
    UnknownOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectTransition {
    pub sequence: u64,
    pub state: EffectState,
    pub occurred_at_unix_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectAttemptTransition {
    pub attempt_no: u64,
    pub sequence: u64,
    pub state: AttemptState,
    pub occurred_at_unix_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectSnapshot {
    pub effect_id: EffectId,
    pub operation_id: OperationId,
    pub input_hash: BlobHash,
    pub state: EffectState,
    pub result_hash: Option<BlobHash>,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionReceipt {
    pub operation_id: OperationId,
    pub input_hash: BlobHash,
    pub result_hash: BlobHash,
    pub result: Vec<u8>,
    pub replayed: bool,
}

/// Durable effect coordinator. `SQLite` and blobs remain outside sandbox rollback state.
pub struct EffectGateway {
    store: Mutex<Store>,
    blobs: BlobStore,
    authorizer: Arc<dyn Authorizer>,
    dispatcher: Arc<dyn EffectDispatcher>,
}

impl EffectGateway {
    /// Opens trusted state and requires explicit authorization and dispatch implementations.
    ///
    /// # Errors
    ///
    /// Returns an error when `SQLite` or the content-addressed blob store cannot be opened.
    pub fn open(
        database_path: impl AsRef<Path>,
        blob_root: impl AsRef<Path>,
        authorizer: Arc<dyn Authorizer>,
        dispatcher: Arc<dyn EffectDispatcher>,
    ) -> Result<Self, GatewayError> {
        Ok(Self {
            store: Mutex::new(Store::open(database_path)?),
            blobs: BlobStore::open(blob_root)?,
            authorizer,
            dispatcher,
        })
    }

    /// Executes or replays one stable operation.
    ///
    /// A committed operation returns its integrity-checked recorded result without authorization
    /// or redispatch. Any other pre-existing operation is fail-closed until explicit recovery.
    ///
    /// # Errors
    ///
    /// Returns typed authorization, conflict, unresolved-outcome, fault, integrity, or storage
    /// errors. Unknown outcomes are never retried automatically.
    pub fn execute(
        &self,
        intent: &CanonicalIntent,
        fault: FaultPoint,
    ) -> Result<ExecutionReceipt, GatewayError> {
        if let Some(existing) = self.find(intent.operation_id())? {
            return self.resolve_existing(intent, existing);
        }

        let authorization = self.authorizer.authorize(&AuthorizationRequest {
            session_id: intent.session_id,
            branch_id: intent.branch_id,
            operation_id: &intent.operation_id,
            action: &intent.action,
            resource: &intent.resource,
            input_hash: &intent.input_hash,
            policy_revision: intent.policy_revision,
        });
        let input_blob = self.blobs.put(
            &intent.canonical_bytes,
            "application/vnd.epoch.effect-intent+json",
        )?;

        if authorization == AuthorizationDecision::Deny {
            self.persist_denial(intent, &input_blob)?;
            return Err(GatewayError::AuthorizationDenied {
                operation_id: intent.operation_id.clone(),
            });
        }

        match self.persist_prepared(intent, &input_blob)? {
            CreateResult::Created(effect_id) => self.dispatch(effect_id, intent, fault),
            CreateResult::Existing(existing) => self.resolve_existing(intent, existing),
        }
    }

    /// Reads the current durable summary for an operation.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation is absent or stored values are invalid.
    pub fn inspect(&self, operation_id: &OperationId) -> Result<EffectSnapshot, GatewayError> {
        self.find(operation_id)?
            .ok_or_else(|| GatewayError::OperationNotFound {
                operation_id: operation_id.clone(),
            })
    }

    /// Reads immutable transition history in sequence order.
    ///
    /// # Errors
    ///
    /// Returns an error for absent operations, invalid stored values, or `SQLite` failures.
    pub fn history(
        &self,
        operation_id: &OperationId,
    ) -> Result<Vec<EffectTransition>, GatewayError> {
        let snapshot = self.inspect(operation_id)?;
        let store = self.store.lock().map_err(|_| GatewayError::LockPoisoned)?;
        let mut statement = store.connection().prepare(
            "SELECT sequence, state, occurred_at_unix_ms \
             FROM effect_transition_history WHERE effect_id = ?1 ORDER BY sequence",
        )?;
        let rows = statement.query_map([snapshot.effect_id.to_string()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (sequence, state, occurred_at_unix_ms) = row?;
            Ok(EffectTransition {
                sequence: u64::try_from(sequence).map_err(|_| {
                    GatewayError::InvalidStoredValue {
                        field: "effect_transition_history.sequence",
                    }
                })?,
                state: parse_history_state(&state)?,
                occurred_at_unix_ms,
            })
        })
        .collect()
    }

    /// Reads immutable dispatch-attempt history in attempt/sequence order.
    ///
    /// # Errors
    ///
    /// Returns an error for absent operations, invalid stored values, or `SQLite` failures.
    pub fn attempt_history(
        &self,
        operation_id: &OperationId,
    ) -> Result<Vec<EffectAttemptTransition>, GatewayError> {
        let snapshot = self.inspect(operation_id)?;
        let store = self.store.lock().map_err(|_| GatewayError::LockPoisoned)?;
        let mut statement = store.connection().prepare(
            "SELECT attempt_no, sequence, state, occurred_at_unix_ms \
             FROM effect_attempt_history WHERE effect_id = ?1 \
             ORDER BY attempt_no, sequence",
        )?;
        let rows = statement.query_map([snapshot.effect_id.to_string()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (attempt_no, sequence, state, occurred_at_unix_ms) = row?;
            Ok(EffectAttemptTransition {
                attempt_no: u64::try_from(attempt_no).map_err(|_| {
                    GatewayError::InvalidStoredValue {
                        field: "effect_attempt_history.attempt_no",
                    }
                })?,
                sequence: u64::try_from(sequence).map_err(|_| {
                    GatewayError::InvalidStoredValue {
                        field: "effect_attempt_history.sequence",
                    }
                })?,
                state: parse_attempt_state(&state)?,
                occurred_at_unix_ms,
            })
        })
        .collect()
    }

    fn dispatch(
        &self,
        effect_id: EffectId,
        intent: &CanonicalIntent,
        fault: FaultPoint,
    ) -> Result<ExecutionReceipt, GatewayError> {
        if fault == FaultPoint::AfterPrepared {
            return Err(GatewayError::FaultInjected {
                point: fault,
                safety: FaultSafety::KnownNotSent,
            });
        }

        self.mark_dispatched(effect_id, intent)?;
        if fault == FaultPoint::AfterDispatchedBeforeInvoke {
            self.mark_unknown(effect_id, 1, "dispatch_boundary")?;
            return Err(GatewayError::FaultInjected {
                point: fault,
                safety: FaultSafety::UnknownOutcome,
            });
        }

        let outcome = self.dispatcher.dispatch(&DispatchRequest {
            operation_id: &intent.operation_id,
            action: &intent.action,
            resource: &intent.resource,
            canonical_input: &intent.canonical_bytes,
            input_hash: &intent.input_hash,
        });
        if fault == FaultPoint::AfterInvokeBeforeCommit {
            self.mark_unknown(effect_id, 1, "response_not_durable")?;
            return Err(GatewayError::FaultInjected {
                point: fault,
                safety: FaultSafety::UnknownOutcome,
            });
        }

        match outcome {
            DispatchOutcome::Committed(result) => self.commit_result(effect_id, intent, result),
            DispatchOutcome::Failed(code) => {
                self.mark_failed(effect_id, 1, code)?;
                Err(GatewayError::DispatchFailed {
                    operation_id: intent.operation_id.clone(),
                    code,
                })
            }
        }
    }

    fn resolve_existing(
        &self,
        intent: &CanonicalIntent,
        existing: EffectSnapshot,
    ) -> Result<ExecutionReceipt, GatewayError> {
        if existing.input_hash != intent.input_hash {
            return Err(GatewayError::OperationInputConflict {
                operation_id: intent.operation_id.clone(),
                recorded: existing.input_hash,
                requested: intent.input_hash.clone(),
            });
        }
        if existing.state != EffectState::Committed {
            return Err(GatewayError::UnresolvedOperation {
                operation_id: intent.operation_id.clone(),
                state: existing.state,
            });
        }
        let result_hash = existing
            .result_hash
            .ok_or(GatewayError::InvalidStoredValue {
                field: "effect_intents.result_hash",
            })?;
        let result = self.blobs.read(&result_hash)?;
        Ok(ExecutionReceipt {
            operation_id: intent.operation_id.clone(),
            input_hash: intent.input_hash.clone(),
            result_hash,
            result,
            replayed: true,
        })
    }

    fn find(&self, operation_id: &OperationId) -> Result<Option<EffectSnapshot>, GatewayError> {
        let store = self.store.lock().map_err(|_| GatewayError::LockPoisoned)?;
        store
            .connection()
            .query_row(
                "SELECT id, operation_id, input_hash, state, result_hash, revision \
                 FROM effect_intents WHERE operation_id = ?1",
                [operation_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                },
            )
            .optional()?
            .map(decode_snapshot)
            .transpose()
    }

    fn persist_denial(
        &self,
        intent: &CanonicalIntent,
        input_blob: &BlobMetadata,
    ) -> Result<(), GatewayError> {
        let now = now_unix_ms()?;
        let effect_id = EffectId::new();
        let mut store = self.store.lock().map_err(|_| GatewayError::LockPoisoned)?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_in_transaction(&transaction, &intent.operation_id)? {
            transaction.rollback()?;
            return self.resolve_existing(intent, existing).map(|_| ());
        }
        insert_blob_metadata(&transaction, input_blob, now)?;
        insert_intent(&transaction, effect_id, intent, "denied", now)?;
        append_transition(
            &transaction,
            effect_id,
            0,
            EffectState::Requested,
            now,
            "{}",
        )?;
        append_transition(
            &transaction,
            effect_id,
            1,
            EffectState::Failed,
            now,
            r#"{"reason":"authorization_denied"}"#,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn persist_prepared(
        &self,
        intent: &CanonicalIntent,
        input_blob: &BlobMetadata,
    ) -> Result<CreateResult, GatewayError> {
        let now = now_unix_ms()?;
        let effect_id = EffectId::new();
        let mut store = self.store.lock().map_err(|_| GatewayError::LockPoisoned)?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = find_in_transaction(&transaction, &intent.operation_id)? {
            transaction.rollback()?;
            return Ok(CreateResult::Existing(existing));
        }
        insert_blob_metadata(&transaction, input_blob, now)?;
        insert_intent(&transaction, effect_id, intent, "prepared", now)?;
        append_transition(
            &transaction,
            effect_id,
            0,
            EffectState::Requested,
            now,
            "{}",
        )?;
        append_transition(&transaction, effect_id, 1, EffectState::Prepared, now, "{}")?;
        transaction.commit()?;
        Ok(CreateResult::Created(effect_id))
    }

    fn mark_dispatched(
        &self,
        effect_id: EffectId,
        intent: &CanonicalIntent,
    ) -> Result<(), GatewayError> {
        let now = now_unix_ms()?;
        let mut store = self.store.lock().map_err(|_| GatewayError::LockPoisoned)?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE effect_intents SET state = 'dispatched', dispatched_at_unix_ms = ?2, \
                    revision = revision + 1 WHERE id = ?1 AND state = 'prepared'",
            params![effect_id.to_string(), now],
        )?;
        if updated != 1 {
            return Err(GatewayError::ConcurrentStateChange {
                operation_id: intent.operation_id.clone(),
            });
        }
        transaction.execute(
            "INSERT INTO effect_attempts \
             (id, effect_id, attempt_no, state, downstream_idempotency_key, started_at_unix_ms) \
             VALUES (?1, ?2, 1, 'started', ?3, ?4)",
            params![
                uuid::Uuid::new_v4().to_string(),
                effect_id.to_string(),
                intent.operation_id.as_str(),
                now,
            ],
        )?;
        append_transition(
            &transaction,
            effect_id,
            2,
            EffectState::Dispatched,
            now,
            "{}",
        )?;
        append_attempt(&transaction, effect_id, 1, 0, "started", now, "{}")?;
        transaction.commit()?;
        Ok(())
    }

    fn mark_unknown(
        &self,
        effect_id: EffectId,
        attempt_no: u64,
        boundary: &'static str,
    ) -> Result<(), GatewayError> {
        let now = now_unix_ms()?;
        let mut store = self.store.lock().map_err(|_| GatewayError::LockPoisoned)?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = serde_json::to_string(&json!({"boundary": boundary}))?;
        update_resolution(&transaction, effect_id, "unknown", now, None, Some(&detail))?;
        update_attempt(
            &transaction,
            effect_id,
            attempt_no,
            "unknown",
            now,
            None,
            Some(&detail),
        )?;
        append_transition(
            &transaction,
            effect_id,
            3,
            EffectState::Unknown,
            now,
            &detail,
        )?;
        append_attempt(
            &transaction,
            effect_id,
            attempt_no,
            1,
            "unknown",
            now,
            &detail,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn mark_failed(
        &self,
        effect_id: EffectId,
        attempt_no: u64,
        code: DispatchFailureCode,
    ) -> Result<(), GatewayError> {
        let now = now_unix_ms()?;
        let mut store = self.store.lock().map_err(|_| GatewayError::LockPoisoned)?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let detail = serde_json::to_string(&json!({"code": failure_code(code)}))?;
        update_resolution(&transaction, effect_id, "failed", now, None, Some(&detail))?;
        update_attempt(
            &transaction,
            effect_id,
            attempt_no,
            "failed",
            now,
            None,
            Some(&detail),
        )?;
        append_transition(
            &transaction,
            effect_id,
            3,
            EffectState::Failed,
            now,
            &detail,
        )?;
        append_attempt(
            &transaction,
            effect_id,
            attempt_no,
            1,
            "failed",
            now,
            &detail,
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn commit_result(
        &self,
        effect_id: EffectId,
        intent: &CanonicalIntent,
        result: DispatchResult,
    ) -> Result<ExecutionReceipt, GatewayError> {
        validate_media_type(&result.media_type)?;
        let result_blob = self.blobs.put(&result.bytes, result.media_type)?;
        let now = now_unix_ms()?;
        let mut store = self.store.lock().map_err(|_| GatewayError::LockPoisoned)?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        insert_blob_metadata(&transaction, &result_blob, now)?;
        update_resolution(
            &transaction,
            effect_id,
            "succeeded",
            now,
            Some(&result_blob.hash),
            None,
        )?;
        update_attempt(
            &transaction,
            effect_id,
            1,
            "succeeded",
            now,
            Some(&result_blob.hash),
            None,
        )?;
        append_transition(
            &transaction,
            effect_id,
            3,
            EffectState::Committed,
            now,
            "{}",
        )?;
        append_attempt(&transaction, effect_id, 1, 1, "committed", now, "{}")?;
        transaction.commit()?;
        Ok(ExecutionReceipt {
            operation_id: intent.operation_id.clone(),
            input_hash: intent.input_hash.clone(),
            result_hash: result_blob.hash,
            result: result.bytes,
            replayed: false,
        })
    }
}

enum CreateResult {
    Created(EffectId),
    Existing(EffectSnapshot),
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), GatewayError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        Err(GatewayError::InvalidField { field })
    } else {
        Ok(())
    }
}

fn validate_media_type(value: &str) -> Result<(), GatewayError> {
    validate_text("dispatch_result.media_type", value, 255)
}

fn reject_sensitive_fields(value: &Value, path: &str) -> Result<(), GatewayError> {
    match value {
        Value::Object(fields) => {
            for (key, nested) in fields {
                let normalized = key.to_ascii_lowercase().replace('-', "_");
                if sensitive_key(&normalized) {
                    return Err(GatewayError::SensitiveField {
                        path: format!("{path}.{key}"),
                    });
                }
                reject_sensitive_fields(nested, &format!("{path}.{key}"))?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                reject_sensitive_fields(item, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn sensitive_key(normalized: &str) -> bool {
    matches!(
        normalized,
        "authorization"
            | "proxy_authorization"
            | "api_key"
            | "apikey"
            | "access_token"
            | "refresh_token"
            | "token"
            | "password"
            | "secret"
            | "credential"
            | "credentials"
            | "client_secret"
    ) || normalized.ends_with("_api_key")
        || normalized.ends_with("_authorization")
        || normalized.ends_with("_access_token")
        || normalized.ends_with("_refresh_token")
}

fn reject_sensitive_text(value: &str, path: &'static str) -> Result<(), GatewayError> {
    const MARKERS: [&str; 10] = [
        "bearer ",
        "basic ",
        "api_key=",
        "api-key=",
        "apikey=",
        "access_token=",
        "access-token=",
        "refresh_token=",
        "client_secret=",
        "password=",
    ];
    let normalized = value.to_ascii_lowercase();
    let has_marker = MARKERS.iter().any(|marker| normalized.contains(marker));
    let has_url_userinfo = normalized.split_once("://").is_some_and(|(_, suffix)| {
        suffix
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|authority| authority.contains('@'))
    });
    if has_marker || has_url_userinfo {
        Err(GatewayError::SensitiveField {
            path: path.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn canonicalize_value(value: Value) -> Value {
    match value {
        Value::Object(fields) => {
            let mut entries = fields.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize_value(value)))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalize_value).collect()),
        scalar => scalar,
    }
}

fn insert_blob_metadata(
    transaction: &Transaction<'_>,
    metadata: &BlobMetadata,
    now: i64,
) -> Result<(), GatewayError> {
    transaction.execute(
        "INSERT INTO blobs (hash, byte_length, media_type, created_at_unix_ms) \
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT(hash) DO NOTHING",
        params![
            metadata.hash.as_str(),
            i64::try_from(metadata.length).map_err(|_| GatewayError::InvalidField {
                field: "blob.length"
            })?,
            metadata.media_type,
            now,
        ],
    )?;
    let recorded: (i64, String) = transaction.query_row(
        "SELECT byte_length, media_type FROM blobs WHERE hash = ?1",
        [metadata.hash.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if recorded
        != (
            i64::try_from(metadata.length).map_err(|_| GatewayError::InvalidField {
                field: "blob.length",
            })?,
            metadata.media_type.clone(),
        )
    {
        return Err(GatewayError::BlobMetadataConflict {
            hash: metadata.hash.clone(),
        });
    }
    Ok(())
}

fn insert_intent(
    transaction: &Transaction<'_>,
    effect_id: EffectId,
    intent: &CanonicalIntent,
    state: &str,
    now: i64,
) -> Result<(), GatewayError> {
    transaction.execute(
        "INSERT INTO effect_intents \
         (id, session_id, branch_id, capability_id, operation_id, replay_key, action, resource, \
          input_hash, state, policy_revision, prepared_at_unix_ms) \
         VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            effect_id.to_string(),
            intent.session_id.to_string(),
            intent.branch_id.to_string(),
            intent.operation_id.as_str(),
            intent.replay_key,
            intent.action,
            intent.resource,
            intent.input_hash.as_str(),
            state,
            i64::try_from(intent.policy_revision).map_err(|_| GatewayError::InvalidField {
                field: "policy_revision"
            })?,
            now,
        ],
    )?;
    Ok(())
}

fn append_transition(
    transaction: &Transaction<'_>,
    effect_id: EffectId,
    sequence: u64,
    state: EffectState,
    now: i64,
    detail_json: &str,
) -> Result<(), GatewayError> {
    transaction.execute(
        "INSERT INTO effect_transition_history \
         (effect_id, sequence, state, occurred_at_unix_ms, detail_json) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            effect_id.to_string(),
            i64::try_from(sequence).map_err(|_| GatewayError::InvalidField {
                field: "transition.sequence"
            })?,
            history_state(state),
            now,
            detail_json,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_attempt(
    transaction: &Transaction<'_>,
    effect_id: EffectId,
    attempt_no: u64,
    sequence: u64,
    state: &str,
    now: i64,
    detail_json: &str,
) -> Result<(), GatewayError> {
    transaction.execute(
        "INSERT INTO effect_attempt_history \
         (effect_id, attempt_no, sequence, state, occurred_at_unix_ms, detail_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            effect_id.to_string(),
            i64::try_from(attempt_no).map_err(|_| GatewayError::InvalidField {
                field: "attempt.number"
            })?,
            i64::try_from(sequence).map_err(|_| GatewayError::InvalidField {
                field: "attempt.sequence"
            })?,
            state,
            now,
            detail_json,
        ],
    )?;
    Ok(())
}

fn update_resolution(
    transaction: &Transaction<'_>,
    effect_id: EffectId,
    state: &str,
    now: i64,
    result_hash: Option<&BlobHash>,
    error_json: Option<&str>,
) -> Result<(), GatewayError> {
    let updated = transaction.execute(
        "UPDATE effect_intents SET state = ?2, result_hash = ?3, error_json = ?4, \
                resolved_at_unix_ms = ?5, revision = revision + 1 \
         WHERE id = ?1 AND state = 'dispatched'",
        params![
            effect_id.to_string(),
            state,
            result_hash.map(BlobHash::as_str),
            error_json,
            now,
        ],
    )?;
    if updated != 1 {
        return Err(GatewayError::InvalidStoredValue {
            field: "effect_intents.state",
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn update_attempt(
    transaction: &Transaction<'_>,
    effect_id: EffectId,
    attempt_no: u64,
    state: &str,
    now: i64,
    response_hash: Option<&BlobHash>,
    error_json: Option<&str>,
) -> Result<(), GatewayError> {
    let updated = transaction.execute(
        "UPDATE effect_attempts SET state = ?3, response_hash = ?4, error_json = ?5, \
                completed_at_unix_ms = ?6 WHERE effect_id = ?1 AND attempt_no = ?2 \
                AND state = 'started'",
        params![
            effect_id.to_string(),
            i64::try_from(attempt_no).map_err(|_| GatewayError::InvalidField {
                field: "attempt.number"
            })?,
            state,
            response_hash.map(BlobHash::as_str),
            error_json,
            now,
        ],
    )?;
    if updated != 1 {
        return Err(GatewayError::InvalidStoredValue {
            field: "effect_attempts.state",
        });
    }
    Ok(())
}

fn find_in_transaction(
    transaction: &Transaction<'_>,
    operation_id: &OperationId,
) -> Result<Option<EffectSnapshot>, GatewayError> {
    transaction
        .query_row(
            "SELECT id, operation_id, input_hash, state, result_hash, revision \
             FROM effect_intents WHERE operation_id = ?1",
            [operation_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?
        .map(decode_snapshot)
        .transpose()
}

fn decode_snapshot(
    stored: (String, String, String, String, Option<String>, i64),
) -> Result<EffectSnapshot, GatewayError> {
    let (effect_id, operation_id, input_hash, state, result_hash, revision) = stored;
    Ok(EffectSnapshot {
        effect_id: effect_id
            .parse()
            .map_err(|_| GatewayError::InvalidStoredValue {
                field: "effect_intents.id",
            })?,
        operation_id: OperationId(operation_id),
        input_hash: BlobHash::from_str(&input_hash)?,
        state: parse_current_state(&state)?,
        result_hash: result_hash
            .map(|value| BlobHash::from_str(&value))
            .transpose()?,
        revision: u64::try_from(revision).map_err(|_| GatewayError::InvalidStoredValue {
            field: "effect_intents.revision",
        })?,
    })
}

fn parse_current_state(value: &str) -> Result<EffectState, GatewayError> {
    match value {
        "prepared" => Ok(EffectState::Prepared),
        "dispatched" => Ok(EffectState::Dispatched),
        "succeeded" => Ok(EffectState::Committed),
        "failed" | "denied" => Ok(EffectState::Failed),
        "unknown" => Ok(EffectState::Unknown),
        _ => Err(GatewayError::InvalidStoredValue {
            field: "effect_intents.state",
        }),
    }
}

fn parse_history_state(value: &str) -> Result<EffectState, GatewayError> {
    match value {
        "requested" => Ok(EffectState::Requested),
        "prepared" => Ok(EffectState::Prepared),
        "dispatched" => Ok(EffectState::Dispatched),
        "committed" => Ok(EffectState::Committed),
        "failed" => Ok(EffectState::Failed),
        "unknown" => Ok(EffectState::Unknown),
        _ => Err(GatewayError::InvalidStoredValue {
            field: "effect_transition_history.state",
        }),
    }
}

fn parse_attempt_state(value: &str) -> Result<AttemptState, GatewayError> {
    match value {
        "started" => Ok(AttemptState::Started),
        "committed" => Ok(AttemptState::Committed),
        "failed" => Ok(AttemptState::Failed),
        "unknown" => Ok(AttemptState::Unknown),
        _ => Err(GatewayError::InvalidStoredValue {
            field: "effect_attempt_history.state",
        }),
    }
}

const fn history_state(state: EffectState) -> &'static str {
    match state {
        EffectState::Requested => "requested",
        EffectState::Prepared => "prepared",
        EffectState::Dispatched => "dispatched",
        EffectState::Committed => "committed",
        EffectState::Failed => "failed",
        EffectState::Unknown => "unknown",
    }
}

const fn failure_code(code: DispatchFailureCode) -> &'static str {
    match code {
        DispatchFailureCode::Rejected => "rejected",
        DispatchFailureCode::Unavailable => "unavailable",
        DispatchFailureCode::InvalidResponse => "invalid_response",
    }
}

fn now_unix_ms() -> Result<i64, GatewayError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| GatewayError::ClockBeforeUnixEpoch)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| GatewayError::ClockOverflow)
}

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("authorization denied for {operation_id}")]
    AuthorizationDenied { operation_id: OperationId },
    #[error("operation {operation_id} reused with different input")]
    OperationInputConflict {
        operation_id: OperationId,
        recorded: BlobHash,
        requested: BlobHash,
    },
    #[error("operation {operation_id} has unresolved state {state:?}")]
    UnresolvedOperation {
        operation_id: OperationId,
        state: EffectState,
    },
    #[error("operation {operation_id} was not found")]
    OperationNotFound { operation_id: OperationId },
    #[error("effect state changed concurrently for {operation_id}")]
    ConcurrentStateChange { operation_id: OperationId },
    #[error("dispatch failed for {operation_id}: {code:?}")]
    DispatchFailed {
        operation_id: OperationId,
        code: DispatchFailureCode,
    },
    #[error("injected fault at {point:?}; safety classification: {safety:?}")]
    FaultInjected {
        point: FaultPoint,
        safety: FaultSafety,
    },
    #[error("sensitive provider credential field is forbidden at {path}")]
    SensitiveField { path: String },
    #[error("invalid effect field: {field}")]
    InvalidField { field: &'static str },
    #[error("invalid trusted stored value: {field}")]
    InvalidStoredValue { field: &'static str },
    #[error("conflicting metadata for content-addressed blob {hash}")]
    BlobMetadataConflict { hash: BlobHash },
    #[error("effect gateway lock was poisoned")]
    LockPoisoned,
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
    #[error("system clock timestamp exceeds SQLite integer range")]
    ClockOverflow,
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Blob(#[from] BlobError),
    #[error(transparent)]
    InvalidBlobHash(#[from] InvalidBlobHash),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
