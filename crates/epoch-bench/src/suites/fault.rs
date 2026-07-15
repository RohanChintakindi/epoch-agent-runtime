use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    str::FromStr as _,
    sync::Arc,
};

use epoch_blob::{BlobHash, BlobStore};
use epoch_capabilities::{
    CapabilityAuthorizer, CapabilityConstraints, CapabilityService, CapabilityState, IssueRequest,
};
use epoch_checkpoint::{
    APPLICATION_CONTEXT_SCHEMA_VERSION, ApplicationCheckpointBackend, ApplicationContext,
    BackendOutcome, CheckpointBackend, MessageRole, ObservableMessage, ResumeCursors,
};
use epoch_core::{BranchId, CapabilityId, SessionId};
use epoch_effects::{
    CanonicalIntent, DeterministicLocalDispatcher, EffectGateway, EffectState, FaultPoint,
    FaultSafety, GatewayError,
};
use epoch_storage::Store;
use epoch_workspace::{RestoreFault, WorkspaceBackend, WorkspaceError, WorkspaceLimits};
use rusqlite::params;
use serde_json::json;
use uuid::Uuid;

use crate::SampleOutcome;

use super::{EvidenceKind, FaultMatrix, FaultRow, SuiteError, bounded};

/// Runs actual Week 2 fault hooks and preserves symbolic external-effect boundaries.
///
/// # Errors
///
/// Returns a setup failure. Injected and unsupported stage outcomes remain matrix rows.
pub fn run_fault_matrix(root: &Path) -> Result<FaultMatrix, SuiteError> {
    if !root.is_absolute() || root.parent().is_none() {
        return Err(SuiteError::InvalidConfig(
            "fault scratch root must be a non-root absolute path".to_owned(),
        ));
    }
    let run_root = root.join(format!("fault-matrix-{}", Uuid::new_v4()));
    let source = run_root.join("source");
    fs::create_dir_all(&source)?;
    fs::write(source.join("first"), b"first")?;
    fs::write(source.join("second"), b"second")?;
    let backend = WorkspaceBackend::open(run_root.join("blobs"), WorkspaceLimits::default())
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let snapshot = backend
        .snapshot(&source)
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let mut rows = Vec::new();
    for (name, fault) in [
        (
            "workspace_restore_after_staging",
            RestoreFault::AfterStagingCreated,
        ),
        (
            "workspace_restore_after_first_entry",
            RestoreFault::AfterFirstEntry,
        ),
        (
            "workspace_restore_before_publish",
            RestoreFault::BeforePublish,
        ),
    ] {
        let target = run_root.join(format!("target-{name}"));
        let outcome = backend.restore_with_fault(&snapshot, &target, fault);
        let rejected = matches!(
            outcome,
            Err(WorkspaceError::FaultInjected { point }) if point == fault
        );
        let staging_clean = staging_entries(&run_root)?.is_empty();
        let containment = rejected && !target.exists() && staging_clean;
        rows.push(FaultRow {
            stage: name.to_owned(),
            evidence_kind: EvidenceKind::Actual,
            outcome: if containment {
                SampleOutcome::Succeeded
            } else {
                SampleOutcome::Failed {
                    error: "workspace fault was not cleanly contained".to_owned(),
                }
            },
            containment_verified: containment,
            claims_external_exactly_once: false,
            evidence: BTreeMap::from([
                ("fault".to_owned(), format!("{fault:?}")),
                ("target_absent".to_owned(), (!target.exists()).to_string()),
                ("staging_clean".to_owned(), staging_clean.to_string()),
            ]),
        });
    }
    rows.push(application_capture_fault(&run_root.join("application"))?);
    rows.extend(effect_fault_campaign(&run_root.join("effects"))?);
    for (stage, detail) in [
        (
            "composite_after_application_before_workspace",
            "the composite checkpoint coordinator has no stage injection API on this revision",
        ),
        (
            "external_provider_reconciliation",
            "the deterministic gateway fixture does not prove a live provider's reconciliation contract",
        ),
    ] {
        rows.push(FaultRow {
            stage: stage.to_owned(),
            evidence_kind: EvidenceKind::Symbolic,
            outcome: SampleOutcome::Unsupported {
                reason: detail.to_owned(),
            },
            containment_verified: false,
            claims_external_exactly_once: false,
            evidence: BTreeMap::from([
                ("api".to_owned(), "unavailable".to_owned()),
                ("claim".to_owned(), "unsupported".to_owned()),
            ]),
        });
    }
    Ok(FaultMatrix { rows })
}

struct EffectHarness {
    database: PathBuf,
    session: SessionId,
    branch: BranchId,
    capability_id: CapabilityId,
    service: Arc<CapabilityService>,
    dispatcher: Arc<DeterministicLocalDispatcher>,
    gateway: EffectGateway,
}

impl EffectHarness {
    fn new(root: &Path, policy_revision: u64) -> Result<Self, SuiteError> {
        fs::create_dir_all(root)?;
        let database = root.join("state.db");
        let blobs = root.join("blobs");
        let session = SessionId::new();
        let branch = BranchId::new();
        let store =
            Store::open(&database).map_err(|error| SuiteError::Backend(error.to_string()))?;
        store
            .connection()
            .execute(
                "INSERT INTO sessions (id, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, 'running', 0, 0)",
                [session.to_string()],
            )
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        store
            .connection()
            .execute(
                "INSERT INTO branches \
                 (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, ?2, 'running', 0, 0)",
                params![branch.to_string(), session.to_string()],
            )
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        drop(store);

        let service = Arc::new(
            CapabilityService::open(&database)
                .map_err(|error| SuiteError::Backend(error.to_string()))?,
        );
        service
            .set_policy_revision(session, branch, policy_revision)
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        let issued = service
            .issue(&IssueRequest {
                session_id: session,
                branch_id: branch,
                subject: "benchmark-agent".to_owned(),
                action: "email.send".to_owned(),
                resource: "mailbox:benchmark".to_owned(),
                constraints: CapabilityConstraints {
                    max_uses: Some(1),
                    budget_units: Some(1),
                },
                expires_at_unix_ms: None,
                policy_revision,
            })
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        let capability_id = issued.capability_id;
        let authorizer = Arc::new(
            CapabilityAuthorizer::new(service.clone(), issued.handle, "benchmark-agent", 1)
                .map_err(|error| SuiteError::Backend(error.to_string()))?,
        );
        let dispatcher = Arc::new(DeterministicLocalDispatcher::default());
        let gateway = EffectGateway::open(&database, &blobs, authorizer, dispatcher.clone())
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        Ok(Self {
            database,
            session,
            branch,
            capability_id,
            service,
            dispatcher,
            gateway,
        })
    }

    fn intent(
        &self,
        replay_key: &str,
        policy_revision: u64,
    ) -> Result<CanonicalIntent, SuiteError> {
        CanonicalIntent::new(
            self.session,
            self.branch,
            replay_key,
            "email.send",
            "mailbox:benchmark",
            json!({"to": "benchmark@example.test", "body": replay_key}),
            policy_revision,
        )
        .map_err(|error| SuiteError::Backend(error.to_string()))
    }
}

fn effect_fault_campaign(root: &Path) -> Result<Vec<FaultRow>, SuiteError> {
    Ok(vec![
        replay_campaign(&root.join("replay"))?,
        unknown_campaign(&root.join("unknown"))?,
        revocation_campaign(&root.join("revocation"))?,
        policy_campaign(&root.join("policy"))?,
    ])
}

fn replay_campaign(root: &Path) -> Result<FaultRow, SuiteError> {
    const ATTEMPTS: usize = 100;
    let harness = EffectHarness::new(root, 1)?;
    let intent = harness.intent("campaign/replay", 1)?;
    let mut result_hash = None;
    let mut correct_receipts = 0_usize;
    for ordinal in 0..ATTEMPTS {
        let receipt = harness
            .gateway
            .execute(&intent, FaultPoint::None)
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        let replay_flag = receipt.replayed == (ordinal != 0);
        let same_result = result_hash
            .as_ref()
            .is_none_or(|expected| expected == &receipt.result_hash);
        if replay_flag && same_result {
            correct_receipts += 1;
        }
        result_hash.get_or_insert(receipt.result_hash);
    }
    let state = harness
        .gateway
        .inspect(intent.operation_id())
        .map_err(|error| SuiteError::Backend(error.to_string()))?
        .state;
    let intent_count: i64 = Store::open(&harness.database)
        .map_err(|error| SuiteError::Backend(error.to_string()))?
        .connection()
        .query_row("SELECT COUNT(*) FROM effect_intents", [], |row| row.get(0))
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let dispatches = harness.dispatcher.dispatch_count();
    let contained = correct_receipts == ATTEMPTS
        && dispatches == 1
        && intent_count == 1
        && state == EffectState::Committed;
    Ok(actual_effect_row(
        "effect_replay_100_runs",
        contained,
        BTreeMap::from([
            ("attempts".to_owned(), ATTEMPTS.to_string()),
            ("correct_receipts".to_owned(), correct_receipts.to_string()),
            ("downstream_dispatches".to_owned(), dispatches.to_string()),
            ("durable_intents".to_owned(), intent_count.to_string()),
            (
                "dispatcher_scope".to_owned(),
                "deterministic_local".to_owned(),
            ),
        ]),
    ))
}

fn unknown_campaign(root: &Path) -> Result<FaultRow, SuiteError> {
    let harness = EffectHarness::new(root, 1)?;
    let intent = harness.intent("campaign/unknown", 1)?;
    let fault = harness
        .gateway
        .execute(&intent, FaultPoint::AfterInvokeBeforeCommit);
    let unknown_fault = matches!(
        fault,
        Err(GatewayError::FaultInjected {
            safety: FaultSafety::UnknownOutcome,
            ..
        })
    );
    let state = harness
        .gateway
        .inspect(intent.operation_id())
        .map_err(|error| SuiteError::Backend(error.to_string()))?
        .state;
    let retry_blocked = matches!(
        harness.gateway.execute(&intent, FaultPoint::None),
        Err(GatewayError::UnresolvedOperation { .. })
    );
    let branch_state: String = Store::open(&harness.database)
        .map_err(|error| SuiteError::Backend(error.to_string()))?
        .connection()
        .query_row(
            "SELECT state FROM branches WHERE id = ?1",
            [harness.branch.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let dispatches = harness.dispatcher.dispatch_count();
    let contained = unknown_fault
        && state == EffectState::Unknown
        && retry_blocked
        && branch_state == "suspended"
        && dispatches == 1;
    Ok(actual_effect_row(
        "effect_unknown_suspends_branch",
        contained,
        BTreeMap::from([
            ("effect_state".to_owned(), format!("{state:?}")),
            ("branch_state".to_owned(), branch_state),
            ("retry_blocked".to_owned(), retry_blocked.to_string()),
            ("downstream_dispatches".to_owned(), dispatches.to_string()),
        ]),
    ))
}

fn revocation_campaign(root: &Path) -> Result<FaultRow, SuiteError> {
    let harness = EffectHarness::new(root, 1)?;
    harness
        .service
        .revoke_by_id(harness.capability_id)
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let resurrection_blocked = Store::open(&harness.database)
        .map_err(|error| SuiteError::Backend(error.to_string()))?
        .connection()
        .execute(
            "UPDATE capabilities SET status = 'active' WHERE id = ?1",
            [harness.capability_id.to_string()],
        )
        .is_err();
    let denied = matches!(
        harness
            .gateway
            .execute(&harness.intent("campaign/revoked", 1)?, FaultPoint::None),
        Err(GatewayError::AuthorizationDenied { .. })
    );
    let state = harness
        .service
        .inspect(harness.capability_id)
        .map_err(|error| SuiteError::Backend(error.to_string()))?
        .state;
    let dispatches = harness.dispatcher.dispatch_count();
    let contained =
        resurrection_blocked && denied && state == CapabilityState::Revoked && dispatches == 0;
    Ok(actual_effect_row(
        "capability_revocation_resurrection_blocked",
        contained,
        BTreeMap::from([
            (
                "raw_resurrection_blocked".to_owned(),
                resurrection_blocked.to_string(),
            ),
            ("gateway_denied".to_owned(), denied.to_string()),
            ("current_state".to_owned(), format!("{state:?}")),
            ("downstream_dispatches".to_owned(), dispatches.to_string()),
        ]),
    ))
}

fn policy_campaign(root: &Path) -> Result<FaultRow, SuiteError> {
    let harness = EffectHarness::new(root, 1)?;
    harness
        .service
        .set_policy_revision(harness.session, harness.branch, 2)
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let rollback_blocked = Store::open(&harness.database)
        .map_err(|error| SuiteError::Backend(error.to_string()))?
        .connection()
        .execute(
            "UPDATE capability_policy_revisions SET current_revision = 1 \
             WHERE session_id = ?1 AND branch_id = ?2",
            params![harness.session.to_string(), harness.branch.to_string()],
        )
        .is_err();
    let denied = matches!(
        harness.gateway.execute(
            &harness.intent("campaign/stale-policy", 1)?,
            FaultPoint::None
        ),
        Err(GatewayError::AuthorizationDenied { .. })
    );
    let current_revision: i64 = Store::open(&harness.database)
        .map_err(|error| SuiteError::Backend(error.to_string()))?
        .connection()
        .query_row(
            "SELECT current_revision FROM capability_policy_revisions \
             WHERE session_id = ?1 AND branch_id = ?2",
            params![harness.session.to_string(), harness.branch.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let dispatches = harness.dispatcher.dispatch_count();
    let contained = rollback_blocked && denied && current_revision == 2 && dispatches == 0;
    Ok(actual_effect_row(
        "capability_policy_rollback_blocked",
        contained,
        BTreeMap::from([
            (
                "raw_policy_rollback_blocked".to_owned(),
                rollback_blocked.to_string(),
            ),
            ("gateway_denied".to_owned(), denied.to_string()),
            (
                "current_policy_revision".to_owned(),
                current_revision.to_string(),
            ),
            ("downstream_dispatches".to_owned(), dispatches.to_string()),
        ]),
    ))
}

fn actual_effect_row(
    stage: &str,
    containment_verified: bool,
    evidence: BTreeMap<String, String>,
) -> FaultRow {
    FaultRow {
        stage: stage.to_owned(),
        evidence_kind: EvidenceKind::Actual,
        outcome: if containment_verified {
            SampleOutcome::Succeeded
        } else {
            SampleOutcome::Failed {
                error: "effect safety invariant failed".to_owned(),
            }
        },
        containment_verified,
        claims_external_exactly_once: false,
        evidence,
    }
}

fn application_capture_fault(root: &Path) -> Result<FaultRow, SuiteError> {
    fs::create_dir_all(root)?;
    let missing = BlobHash::from_str(&"f".repeat(64))
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let context = ApplicationContext {
        schema_version: APPLICATION_CONTEXT_SCHEMA_VERSION,
        safe_point_id: "fault-safe-point".to_owned(),
        deterministic_seed: 1,
        context_revision: 1,
        cursors: ResumeCursors {
            boundary_sequence: 1,
            message_cursor: 1,
            tool_cursor: 0,
            task_cursor: 0,
        },
        model_identifier: "recorded-fault-model".to_owned(),
        tool_registry: BTreeMap::new(),
        messages: vec![ObservableMessage {
            message_id: "missing".to_owned(),
            role: MessageRole::Tool,
            content_hash: missing,
        }],
        pending_tasks: Vec::new(),
        pending_model_request_ids: Vec::new(),
        pending_tool_call_ids: Vec::new(),
        user_visible_summary_hash: None,
    };
    let backend = ApplicationCheckpointBackend::new(
        BlobStore::open(root.join("blobs"))
            .map_err(|error| SuiteError::Backend(error.to_string()))?,
    );
    let outcome = backend.capture(&context);
    let (contained, detail) = match outcome {
        BackendOutcome::Failed(issue) => (true, issue.detail),
        BackendOutcome::Unsupported(issue) => (false, issue.detail),
        BackendOutcome::Supported(_) => (
            false,
            "missing application reference was committed".to_owned(),
        ),
    };
    Ok(FaultRow {
        stage: "application_capture_missing_reference".to_owned(),
        evidence_kind: EvidenceKind::Actual,
        outcome: if contained {
            SampleOutcome::Succeeded
        } else {
            SampleOutcome::Failed {
                error: bounded(&detail),
            }
        },
        containment_verified: contained,
        claims_external_exactly_once: false,
        evidence: BTreeMap::from([
            ("validation_stage".to_owned(), "capture".to_owned()),
            ("diagnostic".to_owned(), bounded(&detail)),
        ]),
    })
}

fn staging_entries(root: &Path) -> Result<Vec<String>, SuiteError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".epoch-restore-") {
            entries.push(name);
        }
    }
    Ok(entries)
}
