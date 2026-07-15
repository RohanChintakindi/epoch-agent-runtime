use std::{collections::BTreeMap, fs, path::Path, str::FromStr as _};

use epoch_blob::{BlobHash, BlobStore};
use epoch_checkpoint::{
    APPLICATION_CONTEXT_SCHEMA_VERSION, ApplicationCheckpointBackend, ApplicationContext,
    BackendOutcome, CheckpointBackend, MessageRole, ObservableMessage, ResumeCursors,
};
use epoch_workspace::{RestoreFault, WorkspaceBackend, WorkspaceError, WorkspaceLimits};
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
    for (stage, detail) in [
        (
            "composite_after_application_before_workspace",
            "the composite checkpoint coordinator has no stage injection API on this revision",
        ),
        (
            "effect_after_downstream_commit_before_local_record",
            "no downstream effect gateway/reconciliation API is present on this revision",
        ),
        (
            "effect_retry_after_restore",
            "rollback evidence cannot establish downstream idempotency or exactly-once delivery",
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
