//! Trusted, branch-bound capability service.
//!
//! Opaque handles are the only authority-bearing value exposed to an untrusted workload. Epoch
//! stores a SHA-256 handle digest and current authority state in its trusted database, outside the
//! checkpoint rollback domain. Every use is checked and consumed in one immediate transaction.

use std::{
    fmt,
    path::Path,
    str::FromStr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use epoch_core::{BranchId, CapabilityId, SessionId};
use epoch_effects::{
    AuthorizationDecision, AuthorizationRequest as EffectAuthorizationRequest, Authorizer,
};
use epoch_storage::{StorageError, Store};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const HANDLE_PREFIX: &str = "ecap_v1_";
const HANDLE_SECRET_HEX_LENGTH: usize = 64;
const MAX_SUBJECT_LENGTH: usize = 255;
const MAX_ACTION_LENGTH: usize = 255;
const MAX_RESOURCE_LENGTH: usize = 2_048;
const MAX_REQUEST_ID_LENGTH: usize = 255;

/// Opaque bearer token returned to the sandbox. Its `Debug` representation never reveals bytes.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct CapabilityHandle(String);

impl CapabilityHandle {
    fn generate() -> Self {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        Self(format!(
            "{HANDLE_PREFIX}{}{}",
            first.simple(),
            second.simple()
        ))
    }

    /// Deliberately exposes the bearer value for delivery across a trusted sandbox boundary.
    /// Callers must not log or persist it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn digest(&self) -> String {
        sha256_hex(self.0.as_bytes())
    }
}

impl fmt::Debug for CapabilityHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CapabilityHandle([REDACTED])")
    }
}

impl FromStr for CapabilityHandle {
    type Err = InvalidCapabilityHandle;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some(secret) = value.strip_prefix(HANDLE_PREFIX) else {
            return Err(InvalidCapabilityHandle);
        };
        if secret.len() != HANDLE_SECRET_HEX_LENGTH
            || !secret
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(InvalidCapabilityHandle);
        }
        Ok(Self(value.to_owned()))
    }
}

/// A syntactically invalid opaque capability handle.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("invalid opaque capability handle")]
pub struct InvalidCapabilityHandle;

/// Supported quantitative constraints. `None` means unbounded at that level.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityConstraints {
    pub max_uses: Option<u64>,
    pub budget_units: Option<u64>,
}

/// Trusted request to issue or attenuate a capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssueRequest {
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub subject: String,
    pub action: String,
    pub resource: String,
    pub constraints: CapabilityConstraints,
    pub expires_at_unix_ms: Option<i64>,
    pub policy_revision: u64,
}

/// Newly issued capability identity and its one-time-delivered opaque handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssuedCapability {
    pub capability_id: CapabilityId,
    pub handle: CapabilityHandle,
}

/// A validated attempt to use a capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityUse {
    session_id: SessionId,
    branch_id: BranchId,
    subject: String,
    action: String,
    resource: String,
    policy_revision: u64,
    budget_units: u64,
    request_id: String,
    request_hash: String,
}

impl CapabilityUse {
    /// Validates a capability-use request before trusted state is consulted.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, control-bearing, zero-budget, or noncanonical fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        branch_id: BranchId,
        subject: impl Into<String>,
        action: impl Into<String>,
        resource: impl Into<String>,
        policy_revision: u64,
        budget_units: u64,
        request_id: impl Into<String>,
        request_hash: &str,
    ) -> Result<Self, CapabilityError> {
        let subject = subject.into();
        let action = action.into();
        let resource = resource.into();
        let request_id = request_id.into();
        validate_text("subject", &subject, MAX_SUBJECT_LENGTH)?;
        validate_text("action", &action, MAX_ACTION_LENGTH)?;
        validate_text("resource", &resource, MAX_RESOURCE_LENGTH)?;
        validate_text("request_id", &request_id, MAX_REQUEST_ID_LENGTH)?;
        validate_digest(request_hash)?;
        if budget_units == 0 {
            return Err(CapabilityError::InvalidField {
                field: "budget_units",
            });
        }
        i64_from_u64("policy_revision", policy_revision)?;
        i64_from_u64("budget_units", budget_units)?;
        Ok(Self {
            session_id,
            branch_id,
            subject,
            action,
            resource,
            policy_revision,
            budget_units,
            request_id,
            request_hash: request_hash.to_owned(),
        })
    }
}

/// Authorization result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionOutcome {
    Allow,
    Deny,
}

/// Stable allow/deny reason recorded in the append-only audit table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DenialReason {
    Allowed,
    UnknownHandle,
    SessionMismatch,
    BranchMismatch,
    SubjectMismatch,
    ActionMismatch,
    ResourceMismatch,
    Revoked,
    Expired,
    Consumed,
    AncestorRevoked,
    AncestorExpired,
    AncestorConsumed,
    PolicyUnavailable,
    PolicyStale,
    PolicyRevisionMismatch,
    BudgetExceeded,
    RequestAlreadyAuthorized,
    CorruptAuthority,
}

impl DenialReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::UnknownHandle => "unknown_handle",
            Self::SessionMismatch => "session_mismatch",
            Self::BranchMismatch => "branch_mismatch",
            Self::SubjectMismatch => "subject_mismatch",
            Self::ActionMismatch => "action_mismatch",
            Self::ResourceMismatch => "resource_mismatch",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
            Self::Consumed => "consumed",
            Self::AncestorRevoked => "ancestor_revoked",
            Self::AncestorExpired => "ancestor_expired",
            Self::AncestorConsumed => "ancestor_consumed",
            Self::PolicyUnavailable => "policy_unavailable",
            Self::PolicyStale => "policy_stale",
            Self::PolicyRevisionMismatch => "policy_revision_mismatch",
            Self::BudgetExceeded => "budget_exceeded",
            Self::RequestAlreadyAuthorized => "request_already_authorized",
            Self::CorruptAuthority => "corrupt_authority",
        }
    }

    fn from_str(value: &str) -> Result<Self, CapabilityError> {
        match value {
            "allowed" => Ok(Self::Allowed),
            "unknown_handle" => Ok(Self::UnknownHandle),
            "session_mismatch" => Ok(Self::SessionMismatch),
            "branch_mismatch" => Ok(Self::BranchMismatch),
            "subject_mismatch" => Ok(Self::SubjectMismatch),
            "action_mismatch" => Ok(Self::ActionMismatch),
            "resource_mismatch" => Ok(Self::ResourceMismatch),
            "revoked" => Ok(Self::Revoked),
            "expired" => Ok(Self::Expired),
            "consumed" => Ok(Self::Consumed),
            "ancestor_revoked" => Ok(Self::AncestorRevoked),
            "ancestor_expired" => Ok(Self::AncestorExpired),
            "ancestor_consumed" => Ok(Self::AncestorConsumed),
            "policy_unavailable" => Ok(Self::PolicyUnavailable),
            "policy_stale" => Ok(Self::PolicyStale),
            "policy_revision_mismatch" => Ok(Self::PolicyRevisionMismatch),
            "budget_exceeded" => Ok(Self::BudgetExceeded),
            "request_already_authorized" => Ok(Self::RequestAlreadyAuthorized),
            "corrupt_authority" => Ok(Self::CorruptAuthority),
            _ => Err(CapabilityError::CorruptRecord {
                field: "capability_decisions.reason",
            }),
        }
    }
}

/// Current decision plus the authoritative capability identity when known.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityDecision {
    pub outcome: DecisionOutcome,
    pub reason: DenialReason,
    pub capability_id: Option<CapabilityId>,
}

/// Durable authorization audit record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityAuditRecord {
    pub sequence: u64,
    pub capability_id: Option<CapabilityId>,
    pub outcome: DecisionOutcome,
    pub reason: DenialReason,
    pub request_id: String,
    pub decided_at_unix_ms: i64,
}

/// Trusted clock. Tests can inject a deterministic implementation.
pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> i64;
}

#[derive(Debug, Default)]
struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .unwrap_or(i64::MAX)
    }
}

/// Trusted durable capability authority.
pub struct CapabilityService {
    store: Mutex<Store>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for CapabilityService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityService")
            .finish_non_exhaustive()
    }
}

impl CapabilityService {
    /// Opens capability authority with the host system clock.
    ///
    /// # Errors
    ///
    /// Returns a storage or migration error.
    pub fn open(database_path: impl AsRef<Path>) -> Result<Self, CapabilityError> {
        Self::open_with_clock(database_path, Arc::new(SystemClock))
    }

    /// Opens capability authority with an injected trusted clock.
    ///
    /// # Errors
    ///
    /// Returns a storage or migration error.
    pub fn open_with_clock(
        database_path: impl AsRef<Path>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, CapabilityError> {
        Ok(Self {
            store: Mutex::new(Store::open(database_path)?),
            clock,
        })
    }

    /// Installs or monotonically advances current policy for one branch.
    ///
    /// # Errors
    ///
    /// Returns an error for revision rollback, an invalid clock, missing branch, or storage failure.
    pub fn set_policy_revision(
        &self,
        session_id: SessionId,
        branch_id: BranchId,
        revision: u64,
    ) -> Result<(), CapabilityError> {
        let revision = i64_from_u64("policy_revision", revision)?;
        let now = self.now()?;
        let mut store = self.lock_store()?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = current_policy(&transaction, session_id, branch_id)?;
        match existing {
            Some(current) if revision < current => {
                return Err(CapabilityError::PolicyRevisionRollback {
                    current: u64::try_from(current).unwrap_or(u64::MAX),
                    requested: u64::try_from(revision).unwrap_or(u64::MAX),
                });
            }
            Some(current) if revision == current => {}
            Some(_) => {
                transaction.execute(
                    "UPDATE capability_policy_revisions \
                     SET current_revision = ?3, updated_at_unix_ms = ?4 \
                     WHERE session_id = ?1 AND branch_id = ?2",
                    params![session_id.to_string(), branch_id.to_string(), revision, now],
                )?;
            }
            None => {
                transaction.execute(
                    "INSERT INTO capability_policy_revisions \
                     (session_id, branch_id, current_revision, updated_at_unix_ms) \
                     VALUES (?1, ?2, ?3, ?4)",
                    params![session_id.to_string(), branch_id.to_string(), revision, now],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Issues a new root capability.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid scope, missing/stale policy, expiration, or storage failure.
    pub fn issue(&self, request: &IssueRequest) -> Result<IssuedCapability, CapabilityError> {
        let now = self.now()?;
        let validated = ValidatedIssue::new(request, now)?;
        let mut store = self.lock_store()?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_current_policy(&transaction, request, validated.policy_revision)?;
        let issued = insert_capability(&transaction, &validated, None)?;
        transaction.execute(
            "INSERT INTO capability_ancestry (capability_id, ancestor_id, depth) \
             VALUES (?1, ?1, 0)",
            [issued.capability_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(issued)
    }

    /// Creates a child handle whose supported scope is no broader than its parent.
    /// Descendant consumption decrements every ancestor, preventing delegated budget expansion.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent is missing/inactive or any supported constraint widens.
    pub fn attenuate(
        &self,
        parent_handle: &CapabilityHandle,
        request: &IssueRequest,
    ) -> Result<IssuedCapability, CapabilityError> {
        let now = self.now()?;
        let validated = ValidatedIssue::new(request, now)?;
        let mut store = self.lock_store()?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let parent = load_capability_by_hash(&transaction, &parent_handle.digest())?
            .ok_or(CapabilityError::UnknownParentCapability)?;
        validate_attenuation(&transaction, &parent, &validated, now)?;
        let issued = insert_capability(&transaction, &validated, Some(parent.id))?;
        transaction.execute(
            "INSERT INTO capability_ancestry (capability_id, ancestor_id, depth) \
             SELECT ?1, ancestor_id, depth + 1 FROM capability_ancestry \
             WHERE capability_id = ?2",
            params![issued.capability_id.to_string(), parent.id.to_string()],
        )?;
        transaction.execute(
            "INSERT INTO capability_ancestry (capability_id, ancestor_id, depth) \
             VALUES (?1, ?1, 0)",
            [issued.capability_id.to_string()],
        )?;
        transaction.commit()?;
        Ok(issued)
    }

    /// Irreversibly revokes a capability. Descendants subsequently fail their ancestor check.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown handle, invalid clock, or storage failure.
    pub fn revoke(&self, handle: &CapabilityHandle) -> Result<(), CapabilityError> {
        let now = self.now()?;
        let mut store = self.lock_store()?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let capability = load_capability_by_hash(&transaction, &handle.digest())?
            .ok_or(CapabilityError::UnknownParentCapability)?;
        if capability.status == CapabilityStatus::Active {
            transaction.execute(
                "UPDATE capabilities SET status = 'revoked', updated_at_unix_ms = ?2 \
                 WHERE id = ?1 AND status = 'active'",
                params![capability.id.to_string(), now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Validates current authority, atomically consumes all ancestor counters, and appends audit.
    /// A deny is a successful, durable policy decision; infrastructure failures are errors.
    ///
    /// # Errors
    ///
    /// Returns an error only when trusted validation or durable storage cannot complete.
    pub fn authorize_and_consume(
        &self,
        handle: &CapabilityHandle,
        request: &CapabilityUse,
    ) -> Result<CapabilityDecision, CapabilityError> {
        let now = self.now()?;
        let handle_hash = handle.digest();
        let mut store = self.lock_store()?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let Some(capability) = load_capability_by_hash(&transaction, &handle_hash)? else {
            return commit_decision(
                transaction,
                None,
                &handle_hash,
                request,
                DecisionOutcome::Deny,
                DenialReason::UnknownHandle,
                now,
            );
        };

        if let Some(reason) = binding_denial(&capability, request) {
            return commit_decision(
                transaction,
                Some(capability.id),
                &handle_hash,
                request,
                DecisionOutcome::Deny,
                reason,
                now,
            );
        }
        if let Some(reason) = direct_state_denial(&transaction, &capability, now)? {
            return commit_decision(
                transaction,
                Some(capability.id),
                &handle_hash,
                request,
                DecisionOutcome::Deny,
                reason,
                now,
            );
        }

        let current_policy =
            current_policy(&transaction, capability.session_id, capability.branch_id)?;
        let policy_reason = match current_policy {
            None => Some(DenialReason::PolicyUnavailable),
            Some(current) if current != capability.policy_revision => {
                Some(DenialReason::PolicyStale)
            }
            Some(current)
                if current != i64_from_u64("policy_revision", request.policy_revision)? =>
            {
                Some(DenialReason::PolicyRevisionMismatch)
            }
            Some(_) => None,
        };
        if let Some(reason) = policy_reason {
            return commit_decision(
                transaction,
                Some(capability.id),
                &handle_hash,
                request,
                DecisionOutcome::Deny,
                reason,
                now,
            );
        }

        let ancestors = load_ancestors(&transaction, capability.id)?;
        if ancestors
            .first()
            .is_none_or(|ancestor| ancestor.id != capability.id)
        {
            return commit_decision(
                transaction,
                Some(capability.id),
                &handle_hash,
                request,
                DecisionOutcome::Deny,
                DenialReason::CorruptAuthority,
                now,
            );
        }
        if let Some(reason) = ancestor_denial(&transaction, &ancestors, now)? {
            return commit_decision(
                transaction,
                Some(capability.id),
                &handle_hash,
                request,
                DecisionOutcome::Deny,
                reason,
                now,
            );
        }

        let already_authorized = transaction
            .query_row(
                "SELECT 1 FROM capability_authorizations \
                 WHERE capability_id = ?1 AND request_id = ?2",
                params![capability.id.to_string(), request.request_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if already_authorized {
            return commit_decision(
                transaction,
                Some(capability.id),
                &handle_hash,
                request,
                DecisionOutcome::Deny,
                DenialReason::RequestAlreadyAuthorized,
                now,
            );
        }

        let requested_budget = i64_from_u64("budget_units", request.budget_units)?;
        if ancestors.iter().any(|ancestor| {
            ancestor
                .remaining_budget_units
                .is_some_and(|remaining| remaining < requested_budget)
        }) {
            return commit_decision(
                transaction,
                Some(capability.id),
                &handle_hash,
                request,
                DecisionOutcome::Deny,
                DenialReason::BudgetExceeded,
                now,
            );
        }

        for ancestor in &ancestors {
            consume_counter(&transaction, ancestor, requested_budget, now)?;
        }
        transaction.execute(
            "INSERT INTO capability_authorizations \
             (capability_id, request_id, request_hash, budget_units, authorized_at_unix_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                capability.id.to_string(),
                request.request_id,
                request.request_hash,
                requested_budget,
                now,
            ],
        )?;
        commit_decision(
            transaction,
            Some(capability.id),
            &handle_hash,
            request,
            DecisionOutcome::Allow,
            DenialReason::Allowed,
            now,
        )
    }

    /// Returns the append-only capability decision history in database sequence order.
    ///
    /// # Errors
    ///
    /// Returns an error if trusted records cannot be read or decoded.
    pub fn audit_history(&self) -> Result<Vec<CapabilityAuditRecord>, CapabilityError> {
        let store = self.lock_store()?;
        let mut statement = store.connection().prepare(
            "SELECT sequence, capability_id, outcome, reason, request_id, decided_at_unix_ms \
             FROM capability_decisions ORDER BY sequence",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (sequence, capability_id, outcome, reason, request_id, decided_at) = row?;
            Ok(CapabilityAuditRecord {
                sequence: u64::try_from(sequence).map_err(|_| CapabilityError::CorruptRecord {
                    field: "capability_decisions.sequence",
                })?,
                capability_id: capability_id
                    .map(|id| parse_capability_id(&id))
                    .transpose()?,
                outcome: match outcome.as_str() {
                    "allow" => DecisionOutcome::Allow,
                    "deny" => DecisionOutcome::Deny,
                    _ => {
                        return Err(CapabilityError::CorruptRecord {
                            field: "capability_decisions.outcome",
                        });
                    }
                },
                reason: DenialReason::from_str(&reason)?,
                request_id,
                decided_at_unix_ms: decided_at,
            })
        })
        .collect()
    }

    fn now(&self) -> Result<i64, CapabilityError> {
        let now = self.clock.now_unix_ms();
        if now < 0 {
            Err(CapabilityError::InvalidClock)
        } else {
            Ok(now)
        }
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, Store>, CapabilityError> {
        self.store.lock().map_err(|_| CapabilityError::LockPoisoned)
    }
}

/// Adapter from current capability authority to the effect gateway's fail-closed seam.
///
/// The adapter owns one opaque handle and subject binding. It never holds a provider credential.
pub struct CapabilityAuthorizer {
    service: Arc<CapabilityService>,
    handle: CapabilityHandle,
    subject: String,
    budget_units: u64,
}

impl fmt::Debug for CapabilityAuthorizer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapabilityAuthorizer")
            .field("handle", &self.handle)
            .field("subject", &self.subject)
            .field("budget_units", &self.budget_units)
            .finish()
    }
}

impl CapabilityAuthorizer {
    /// Constructs an effect authorizer bound to a single opaque handle and subject.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid subject or zero/oversized budget charge.
    pub fn new(
        service: Arc<CapabilityService>,
        handle: CapabilityHandle,
        subject: impl Into<String>,
        budget_units: u64,
    ) -> Result<Self, CapabilityError> {
        let subject = subject.into();
        validate_text("subject", &subject, MAX_SUBJECT_LENGTH)?;
        if budget_units == 0 {
            return Err(CapabilityError::InvalidField {
                field: "budget_units",
            });
        }
        i64_from_u64("budget_units", budget_units)?;
        Ok(Self {
            service,
            handle,
            subject,
            budget_units,
        })
    }
}

impl Authorizer for CapabilityAuthorizer {
    fn authorize(&self, request: &EffectAuthorizationRequest<'_>) -> AuthorizationDecision {
        let capability_use = CapabilityUse::new(
            request.session_id,
            request.branch_id,
            &self.subject,
            request.action,
            request.resource,
            request.policy_revision,
            self.budget_units,
            request.operation_id.as_str(),
            request.input_hash.as_str(),
        );
        match capability_use.and_then(|capability_use| {
            self.service
                .authorize_and_consume(&self.handle, &capability_use)
        }) {
            Ok(CapabilityDecision {
                outcome: DecisionOutcome::Allow,
                ..
            }) => AuthorizationDecision::Allow,
            Ok(_) | Err(_) => AuthorizationDecision::Deny,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapabilityStatus {
    Active,
    Consumed,
    Expired,
    Revoked,
}

impl CapabilityStatus {
    fn parse(value: &str) -> Result<Self, CapabilityError> {
        match value {
            "active" => Ok(Self::Active),
            "consumed" => Ok(Self::Consumed),
            "expired" => Ok(Self::Expired),
            "revoked" => Ok(Self::Revoked),
            _ => Err(CapabilityError::CorruptRecord {
                field: "capabilities.status",
            }),
        }
    }
}

#[derive(Debug)]
struct StoredCapability {
    id: CapabilityId,
    session_id: SessionId,
    branch_id: BranchId,
    subject: String,
    action: String,
    resource: String,
    remaining_uses: Option<i64>,
    remaining_budget_units: Option<i64>,
    policy_revision: i64,
    status: CapabilityStatus,
    expires_at_unix_ms: Option<i64>,
}

#[derive(Debug)]
struct ValidatedIssue {
    session_id: SessionId,
    branch_id: BranchId,
    subject: String,
    action: String,
    resource: String,
    constraints_json: String,
    remaining_uses: Option<i64>,
    remaining_budget_units: Option<i64>,
    expires_at_unix_ms: Option<i64>,
    policy_revision: i64,
    issued_at_unix_ms: i64,
}

impl ValidatedIssue {
    fn new(request: &IssueRequest, now: i64) -> Result<Self, CapabilityError> {
        validate_text("subject", &request.subject, MAX_SUBJECT_LENGTH)?;
        validate_text("action", &request.action, MAX_ACTION_LENGTH)?;
        validate_text("resource", &request.resource, MAX_RESOURCE_LENGTH)?;
        let remaining_uses = request
            .constraints
            .max_uses
            .map(|value| positive_i64("max_uses", value))
            .transpose()?;
        let remaining_budget_units = request
            .constraints
            .budget_units
            .map(|value| positive_i64("budget_units", value))
            .transpose()?;
        if request
            .expires_at_unix_ms
            .is_some_and(|expires| expires <= now)
        {
            return Err(CapabilityError::InvalidExpiration);
        }
        let constraints_json = serde_json::to_string(&json!({
            "budget_units": request.constraints.budget_units,
            "max_uses": request.constraints.max_uses,
        }))?;
        Ok(Self {
            session_id: request.session_id,
            branch_id: request.branch_id,
            subject: request.subject.clone(),
            action: request.action.clone(),
            resource: request.resource.clone(),
            constraints_json,
            remaining_uses,
            remaining_budget_units,
            expires_at_unix_ms: request.expires_at_unix_ms,
            policy_revision: i64_from_u64("policy_revision", request.policy_revision)?,
            issued_at_unix_ms: now,
        })
    }
}

fn require_current_policy(
    transaction: &Transaction<'_>,
    request: &IssueRequest,
    requested_revision: i64,
) -> Result<(), CapabilityError> {
    let current = current_policy(transaction, request.session_id, request.branch_id)?
        .ok_or(CapabilityError::PolicyNotInitialized)?;
    if current == requested_revision {
        Ok(())
    } else {
        Err(CapabilityError::PolicyNotCurrent {
            current: u64::try_from(current).unwrap_or(u64::MAX),
            requested: request.policy_revision,
        })
    }
}

fn insert_capability(
    transaction: &Transaction<'_>,
    request: &ValidatedIssue,
    delegated_from_id: Option<CapabilityId>,
) -> Result<IssuedCapability, CapabilityError> {
    let capability_id = CapabilityId::new();
    let handle = CapabilityHandle::generate();
    transaction.execute(
        "INSERT INTO capabilities \
         (id, session_id, branch_id, subject, action, resource, constraints_json, handle_hash, \
          delegated_from_id, remaining_uses, policy_revision, status, issued_at_unix_ms, \
          expires_at_unix_ms, updated_at_unix_ms, remaining_budget_units) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'active', ?12, ?13, ?12, ?14)",
        params![
            capability_id.to_string(),
            request.session_id.to_string(),
            request.branch_id.to_string(),
            request.subject,
            request.action,
            request.resource,
            request.constraints_json,
            handle.digest(),
            delegated_from_id.map(|id| id.to_string()),
            request.remaining_uses,
            request.policy_revision,
            request.issued_at_unix_ms,
            request.expires_at_unix_ms,
            request.remaining_budget_units,
        ],
    )?;
    Ok(IssuedCapability {
        capability_id,
        handle,
    })
}

fn validate_attenuation(
    transaction: &Transaction<'_>,
    parent: &StoredCapability,
    child: &ValidatedIssue,
    now: i64,
) -> Result<(), CapabilityError> {
    if parent.status != CapabilityStatus::Active {
        return Err(CapabilityError::InactiveParentCapability);
    }
    if parent
        .expires_at_unix_ms
        .is_some_and(|expires| now >= expires)
    {
        return Err(CapabilityError::InactiveParentCapability);
    }
    if parent.session_id != child.session_id {
        return Err(CapabilityError::AttenuationWouldWiden {
            field: "session_id",
        });
    }
    if parent.branch_id != child.branch_id {
        return Err(CapabilityError::AttenuationWouldWiden { field: "branch_id" });
    }
    if parent.subject != child.subject {
        return Err(CapabilityError::AttenuationWouldWiden { field: "subject" });
    }
    if parent.action != child.action {
        return Err(CapabilityError::AttenuationWouldWiden { field: "action" });
    }
    if parent.resource != child.resource {
        return Err(CapabilityError::AttenuationWouldWiden { field: "resource" });
    }
    if parent.policy_revision != child.policy_revision {
        return Err(CapabilityError::AttenuationWouldWiden {
            field: "policy_revision",
        });
    }
    require_narrower_limit(child.remaining_uses, parent.remaining_uses, "max_uses")?;
    require_narrower_limit(
        child.remaining_budget_units,
        parent.remaining_budget_units,
        "budget_units",
    )?;
    if parent.expires_at_unix_ms.is_some_and(|parent_expiry| {
        child
            .expires_at_unix_ms
            .is_none_or(|child_expiry| child_expiry > parent_expiry)
    }) {
        return Err(CapabilityError::AttenuationWouldWiden {
            field: "expires_at_unix_ms",
        });
    }
    let current = current_policy(transaction, parent.session_id, parent.branch_id)?
        .ok_or(CapabilityError::PolicyNotInitialized)?;
    if current != parent.policy_revision {
        return Err(CapabilityError::InactiveParentCapability);
    }
    Ok(())
}

fn require_narrower_limit(
    child: Option<i64>,
    parent: Option<i64>,
    field: &'static str,
) -> Result<(), CapabilityError> {
    if parent.is_some_and(|parent| child.is_none_or(|child| child > parent)) {
        Err(CapabilityError::AttenuationWouldWiden { field })
    } else {
        Ok(())
    }
}

fn binding_denial(capability: &StoredCapability, request: &CapabilityUse) -> Option<DenialReason> {
    if capability.session_id != request.session_id {
        Some(DenialReason::SessionMismatch)
    } else if capability.branch_id != request.branch_id {
        Some(DenialReason::BranchMismatch)
    } else if capability.subject != request.subject {
        Some(DenialReason::SubjectMismatch)
    } else if capability.action != request.action {
        Some(DenialReason::ActionMismatch)
    } else if capability.resource != request.resource {
        Some(DenialReason::ResourceMismatch)
    } else {
        None
    }
}

fn direct_state_denial(
    transaction: &Transaction<'_>,
    capability: &StoredCapability,
    now: i64,
) -> Result<Option<DenialReason>, CapabilityError> {
    match capability.status {
        CapabilityStatus::Revoked => return Ok(Some(DenialReason::Revoked)),
        CapabilityStatus::Expired => return Ok(Some(DenialReason::Expired)),
        CapabilityStatus::Consumed => return Ok(Some(DenialReason::Consumed)),
        CapabilityStatus::Active => {}
    }
    if capability
        .expires_at_unix_ms
        .is_some_and(|expires| now >= expires)
    {
        mark_expired(transaction, capability.id, now)?;
        Ok(Some(DenialReason::Expired))
    } else {
        Ok(None)
    }
}

fn ancestor_denial(
    transaction: &Transaction<'_>,
    ancestors: &[StoredCapability],
    now: i64,
) -> Result<Option<DenialReason>, CapabilityError> {
    for ancestor in ancestors.iter().skip(1) {
        match ancestor.status {
            CapabilityStatus::Revoked => return Ok(Some(DenialReason::AncestorRevoked)),
            CapabilityStatus::Expired => return Ok(Some(DenialReason::AncestorExpired)),
            CapabilityStatus::Consumed => return Ok(Some(DenialReason::AncestorConsumed)),
            CapabilityStatus::Active => {}
        }
        if ancestor
            .expires_at_unix_ms
            .is_some_and(|expires| now >= expires)
        {
            mark_expired(transaction, ancestor.id, now)?;
            return Ok(Some(DenialReason::AncestorExpired));
        }
    }
    Ok(None)
}

fn consume_counter(
    transaction: &Transaction<'_>,
    capability: &StoredCapability,
    budget_units: i64,
    now: i64,
) -> Result<(), CapabilityError> {
    let remaining_uses = capability.remaining_uses.map(|remaining| remaining - 1);
    let remaining_budget = capability
        .remaining_budget_units
        .map(|remaining| remaining - budget_units);
    let status = if remaining_uses == Some(0) || remaining_budget == Some(0) {
        "consumed"
    } else {
        "active"
    };
    let changed = transaction.execute(
        "UPDATE capabilities SET remaining_uses = ?2, remaining_budget_units = ?3, \
         status = ?4, updated_at_unix_ms = ?5 WHERE id = ?1 AND status = 'active'",
        params![
            capability.id.to_string(),
            remaining_uses,
            remaining_budget,
            status,
            now,
        ],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(CapabilityError::ConcurrentAuthorityChange)
    }
}

fn mark_expired(
    transaction: &Transaction<'_>,
    capability_id: CapabilityId,
    now: i64,
) -> Result<(), CapabilityError> {
    transaction.execute(
        "UPDATE capabilities SET status = 'expired', updated_at_unix_ms = ?2 \
         WHERE id = ?1 AND status = 'active'",
        params![capability_id.to_string(), now],
    )?;
    Ok(())
}

fn commit_decision(
    transaction: Transaction<'_>,
    capability_id: Option<CapabilityId>,
    handle_hash: &str,
    request: &CapabilityUse,
    outcome: DecisionOutcome,
    reason: DenialReason,
    now: i64,
) -> Result<CapabilityDecision, CapabilityError> {
    transaction.execute(
        "INSERT INTO capability_decisions \
         (decision_id, capability_id, handle_hash, session_id, branch_id, subject, action, \
          resource, request_id, request_hash, policy_revision, budget_units, outcome, reason, \
          decided_at_unix_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            Uuid::new_v4().to_string(),
            capability_id.map(|id| id.to_string()),
            handle_hash,
            request.session_id.to_string(),
            request.branch_id.to_string(),
            request.subject,
            request.action,
            request.resource,
            request.request_id,
            request.request_hash,
            i64_from_u64("policy_revision", request.policy_revision)?,
            i64_from_u64("budget_units", request.budget_units)?,
            match outcome {
                DecisionOutcome::Allow => "allow",
                DecisionOutcome::Deny => "deny",
            },
            reason.as_str(),
            now,
        ],
    )?;
    transaction.commit()?;
    Ok(CapabilityDecision {
        outcome,
        reason,
        capability_id,
    })
}

fn current_policy(
    connection: &Connection,
    session_id: SessionId,
    branch_id: BranchId,
) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT current_revision FROM capability_policy_revisions \
             WHERE session_id = ?1 AND branch_id = ?2",
            params![session_id.to_string(), branch_id.to_string()],
            |row| row.get(0),
        )
        .optional()
}

fn load_capability_by_hash(
    connection: &Connection,
    handle_hash: &str,
) -> Result<Option<StoredCapability>, CapabilityError> {
    let raw = connection
        .query_row(
            "SELECT id, session_id, branch_id, subject, action, resource, remaining_uses, \
             remaining_budget_units, policy_revision, status, expires_at_unix_ms \
             FROM capabilities WHERE handle_hash = ?1",
            [handle_hash],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                ))
            },
        )
        .optional()?;
    raw.map(stored_capability_from_raw).transpose()
}

fn load_ancestors(
    connection: &Connection,
    capability_id: CapabilityId,
) -> Result<Vec<StoredCapability>, CapabilityError> {
    let mut statement = connection.prepare(
        "SELECT c.id, c.session_id, c.branch_id, c.subject, c.action, c.resource, \
         c.remaining_uses, c.remaining_budget_units, c.policy_revision, c.status, \
         c.expires_at_unix_ms \
         FROM capability_ancestry a JOIN capabilities c ON c.id = a.ancestor_id \
         WHERE a.capability_id = ?1 ORDER BY a.depth",
    )?;
    let rows = statement.query_map([capability_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<i64>>(7)?,
            row.get::<_, i64>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, Option<i64>>(10)?,
        ))
    })?;
    rows.map(|row| stored_capability_from_raw(row?)).collect()
}

type RawCapability = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<i64>,
    Option<i64>,
    i64,
    String,
    Option<i64>,
);

fn stored_capability_from_raw(raw: RawCapability) -> Result<StoredCapability, CapabilityError> {
    Ok(StoredCapability {
        id: parse_capability_id(&raw.0)?,
        session_id: raw.1.parse().map_err(|_| CapabilityError::CorruptRecord {
            field: "capabilities.session_id",
        })?,
        branch_id: raw.2.parse().map_err(|_| CapabilityError::CorruptRecord {
            field: "capabilities.branch_id",
        })?,
        subject: raw.3,
        action: raw.4,
        resource: raw.5,
        remaining_uses: raw.6,
        remaining_budget_units: raw.7,
        policy_revision: raw.8,
        status: CapabilityStatus::parse(&raw.9)?,
        expires_at_unix_ms: raw.10,
    })
}

fn parse_capability_id(value: &str) -> Result<CapabilityId, CapabilityError> {
    value.parse().map_err(|_| CapabilityError::CorruptRecord {
        field: "capabilities.id",
    })
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), CapabilityError> {
    if value.is_empty()
        || value.len() > maximum
        || value.chars().any(char::is_control)
        || value.trim() != value
    {
        Err(CapabilityError::InvalidField { field })
    } else {
        Ok(())
    }
}

fn validate_digest(value: &str) -> Result<(), CapabilityError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(CapabilityError::InvalidField {
            field: "request_hash",
        })
    }
}

fn positive_i64(field: &'static str, value: u64) -> Result<i64, CapabilityError> {
    if value == 0 {
        Err(CapabilityError::InvalidField { field })
    } else {
        i64_from_u64(field, value)
    }
}

fn i64_from_u64(field: &'static str, value: u64) -> Result<i64, CapabilityError> {
    i64::try_from(value).map_err(|_| CapabilityError::InvalidField { field })
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

/// Capability service failure. Policy denials are returned as durable decisions, not errors.
#[derive(Debug, Error)]
pub enum CapabilityError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid capability field: {field}")]
    InvalidField { field: &'static str },
    #[error("capability expiration must be in the future")]
    InvalidExpiration,
    #[error("trusted clock returned a pre-epoch timestamp")]
    InvalidClock,
    #[error("capability policy has not been initialized for this branch")]
    PolicyNotInitialized,
    #[error("policy revision rollback from {current} to {requested} is forbidden")]
    PolicyRevisionRollback { current: u64, requested: u64 },
    #[error("policy revision {requested} is stale; current revision is {current}")]
    PolicyNotCurrent { current: u64, requested: u64 },
    #[error("parent capability is unknown")]
    UnknownParentCapability,
    #[error("parent capability is not currently active")]
    InactiveParentCapability,
    #[error("attenuation would widen {field}")]
    AttenuationWouldWiden { field: &'static str },
    #[error("capability record is corrupt: {field}")]
    CorruptRecord { field: &'static str },
    #[error("capability state changed concurrently")]
    ConcurrentAuthorityChange,
    #[error("capability store lock was poisoned")]
    LockPoisoned,
}
