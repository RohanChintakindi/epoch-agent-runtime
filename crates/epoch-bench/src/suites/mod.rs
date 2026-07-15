mod checkpoint;
mod cow;
mod fault;

use std::{collections::BTreeMap, fmt::Write as _, path::PathBuf, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{BenchmarkEnvironment, BenchmarkReport, PercentileSummary, SampleOutcome};

pub use checkpoint::{run_checkpoint_suite, run_compatibility_matrix};
pub use cow::run_cow_experiment;
pub use fault::run_fault_matrix;

const MAX_SUITE_WARMUPS: u32 = 100;
const MAX_SUITE_REPETITIONS: u32 = 1_000;
const MAX_FIXTURE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FIXTURE_FILES: u32 = 4_096;
const MAX_COW_ALLOCATION_BYTES: u64 = 256 * 1024 * 1024;
const MAX_COW_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COW_CHILDREN: u32 = 16;
const MAX_COW_REPETITIONS: u32 = 100;

/// Stable benchmark suite selector.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuiteName {
    /// Combined application and workspace checkpoint/restore.
    Checkpoint,
    /// Linux copy-on-write process-memory experiment.
    Cow,
    /// Compatibility and scaling matrix.
    Compatibility,
    /// Checkpoint and effect-boundary fault matrix.
    Faults,
    /// Every required Week 4 suite.
    All,
}

impl SuiteName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::Cow => "cow",
            Self::Compatibility => "compatibility",
            Self::Faults => "faults",
            Self::All => "all",
        }
    }
}

impl FromStr for SuiteName {
    type Err = SuiteError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "checkpoint" => Ok(Self::Checkpoint),
            "cow" => Ok(Self::Cow),
            "compatibility" => Ok(Self::Compatibility),
            "faults" => Ok(Self::Faults),
            "all" => Ok(Self::All),
            _ => Err(SuiteError::InvalidConfig(format!(
                "unknown benchmark suite {value:?}; expected checkpoint, cow, compatibility, faults, or all"
            ))),
        }
    }
}

/// Bounded combined application/workspace suite configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointSuiteConfig {
    /// Dedicated scratch root.
    pub root: PathBuf,
    /// Deterministic root seed.
    pub seed: u64,
    /// Discarded samples.
    pub warmups: u32,
    /// Retained samples per trace mode.
    pub repetitions: u32,
    /// Total fixture file bytes.
    pub fixture_bytes: u64,
    /// Number of fixture files.
    pub fixture_files: u32,
}

impl CheckpointSuiteConfig {
    /// Validates resource and path bounds.
    ///
    /// # Errors
    ///
    /// Returns a configuration error before allocating or writing resources.
    pub fn validate(&self) -> Result<(), SuiteError> {
        if !self.root.is_absolute() {
            return Err(SuiteError::InvalidConfig(
                "checkpoint scratch root must be absolute".to_owned(),
            ));
        }
        if self.warmups > MAX_SUITE_WARMUPS {
            return Err(SuiteError::InvalidConfig(format!(
                "warmups exceed {MAX_SUITE_WARMUPS}"
            )));
        }
        if self.repetitions == 0 || self.repetitions > MAX_SUITE_REPETITIONS {
            return Err(SuiteError::InvalidConfig(format!(
                "repetitions must be between 1 and {MAX_SUITE_REPETITIONS}"
            )));
        }
        if self.fixture_bytes == 0 || self.fixture_bytes > MAX_FIXTURE_BYTES {
            return Err(SuiteError::InvalidConfig(format!(
                "fixture bytes must be between 1 and {MAX_FIXTURE_BYTES}"
            )));
        }
        if self.fixture_files == 0 || self.fixture_files > MAX_FIXTURE_FILES {
            return Err(SuiteError::InvalidConfig(format!(
                "fixture files must be between 1 and {MAX_FIXTURE_FILES}"
            )));
        }
        if u64::from(self.fixture_files) > self.fixture_bytes {
            return Err(SuiteError::InvalidConfig(
                "fixture must provide at least one byte per file".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Validated Linux COW experiment configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CowConfig {
    /// Anonymous allocation touched before fork.
    pub allocation_bytes: u64,
    /// Number of forked children.
    pub child_fanout: u32,
    /// Per-child dirty ratio in basis points (0-10,000).
    pub dirty_ratio_basis_points: u32,
    /// Retained experiment repetitions.
    pub repetitions: u32,
}

impl CowConfig {
    /// Constructs a resource-bounded COW configuration.
    ///
    /// # Errors
    ///
    /// Rejects zero, overflowing, or dangerous allocation/fan-out combinations.
    pub fn new(
        allocation_bytes: u64,
        child_fanout: u32,
        dirty_ratio_basis_points: u32,
        repetitions: u32,
    ) -> Result<Self, SuiteError> {
        let config = Self {
            allocation_bytes,
            child_fanout,
            dirty_ratio_basis_points,
            repetitions,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), SuiteError> {
        if !(4_096..=MAX_COW_ALLOCATION_BYTES).contains(&self.allocation_bytes) {
            return Err(SuiteError::InvalidConfig(format!(
                "COW allocation must be 4096 through {MAX_COW_ALLOCATION_BYTES} bytes"
            )));
        }
        if self.child_fanout == 0 || self.child_fanout > MAX_COW_CHILDREN {
            return Err(SuiteError::InvalidConfig(format!(
                "COW child fan-out must be 1 through {MAX_COW_CHILDREN}"
            )));
        }
        let total = self
            .allocation_bytes
            .checked_mul(u64::from(self.child_fanout))
            .ok_or_else(|| SuiteError::InvalidConfig("COW allocation overflows".to_owned()))?;
        if total > MAX_COW_TOTAL_BYTES {
            return Err(SuiteError::InvalidConfig(format!(
                "COW allocation times fan-out exceeds {MAX_COW_TOTAL_BYTES} bytes"
            )));
        }
        if self.dirty_ratio_basis_points > 10_000 {
            return Err(SuiteError::InvalidConfig(
                "COW dirty ratio exceeds 10000 basis points".to_owned(),
            ));
        }
        if self.repetitions == 0 || self.repetitions > MAX_COW_REPETITIONS {
            return Err(SuiteError::InvalidConfig(format!(
                "COW repetitions must be 1 through {MAX_COW_REPETITIONS}"
            )));
        }
        Ok(())
    }
}

/// Predeclared evidence thresholds used by the decision report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DecisionThresholds {
    /// Maximum accepted p95 combined capture time.
    pub checkpoint_capture_p95_ns: u64,
    /// Maximum accepted p95 combined restore time.
    pub checkpoint_restore_p95_ns: u64,
    /// Maximum accepted combined checkpoint validation failures.
    pub checkpoint_validation_failures: u32,
    /// Maximum accepted COW PSS/full-copy byte ratio in basis points.
    pub cow_pss_ratio_basis_points: u32,
}

impl DecisionThresholds {
    /// Thresholds frozen before collection for the Week 4 prototype.
    #[must_use]
    pub const fn week4() -> Self {
        Self {
            checkpoint_capture_p95_ns: 2_000_000_000,
            checkpoint_restore_p95_ns: 2_000_000_000,
            checkpoint_validation_failures: 0,
            cow_pss_ratio_basis_points: 7_500,
        }
    }
}

/// Complete suite request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SuiteRequest {
    /// Suite selector.
    pub suite: SuiteName,
    /// Checkpoint/matrix configuration.
    pub checkpoint: CheckpointSuiteConfig,
    /// COW configuration.
    pub cow: CowConfig,
    /// Predeclared decision thresholds.
    pub thresholds: DecisionThresholds,
}

/// One correctness validation performed alongside timed samples.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ValidationCase {
    /// Stable case name.
    pub name: String,
    /// Whether the validation passed.
    pub passed: bool,
    /// Bounded evidence.
    pub detail: String,
}

/// Combined checkpoint suite evidence.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CheckpointSuiteEvidence {
    /// Separate trace-off and trace-on benchmark reports.
    pub reports: Vec<BenchmarkReport>,
    /// Untimed correctness and rejection validations.
    pub validation_cases: Vec<ValidationCase>,
}

/// One compatibility or scaling matrix row.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CompatibilityRow {
    /// Stable case name.
    pub case: String,
    /// Component/backend under test.
    pub component: String,
    /// Exact configuration facts.
    pub configuration: BTreeMap<String, String>,
    /// Preserved result, including unsupported and failed cases.
    pub outcome: SampleOutcome,
    /// Duration when the case executed.
    pub elapsed_ns: u64,
    /// Evidence references or observed values.
    pub evidence: BTreeMap<String, String>,
}

/// Complete checkpoint compatibility and scaling matrix.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CompatibilityMatrix {
    /// Environment facts shared by all rows.
    pub environment: BenchmarkEnvironment,
    /// Every configured row, including failures and unsupported cases.
    pub rows: Vec<CompatibilityRow>,
}

/// One raw COW helper observation.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CowSample {
    /// Retained ordinal.
    pub ordinal: u32,
    /// Helper wall time.
    pub elapsed_ns: u64,
    /// Total minor faults reported by children.
    pub minor_faults: u64,
    /// Total major faults reported by children.
    pub major_faults: u64,
    /// Parent and child proportional-set bytes.
    pub cow_pss_bytes: u64,
    /// Parent and child resident-set bytes.
    pub cow_rss_bytes: u64,
    /// Bytes allocated by a real full-copy control.
    pub full_copy_bytes: u64,
    /// Full-copy control duration.
    pub full_copy_ns: u64,
}

/// COW experiment evidence or structured unsupported outcome.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CowEvidence {
    /// Configuration.
    pub config: CowConfig,
    /// Environment.
    pub environment: BenchmarkEnvironment,
    /// Overall classification.
    pub outcome: SampleOutcome,
    /// Raw successful samples.
    pub samples: Vec<CowSample>,
    /// Aggregate raw-sample summary, absent for unsupported or failed experiments.
    pub summary: Option<CowSummary>,
}

/// COW raw-sample aggregate summary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CowSummary {
    /// Helper wall-time percentiles.
    pub elapsed_ns: PercentileSummary,
    /// COW proportional-set byte percentiles.
    pub cow_pss_bytes: PercentileSummary,
    /// Full-copy control byte percentiles.
    pub full_copy_bytes: PercentileSummary,
    /// Minor page-fault percentiles.
    pub minor_faults: PercentileSummary,
    /// Major page-fault percentiles.
    pub major_faults: PercentileSummary,
    /// Aggregate COW PSS divided by aggregate full-copy bytes, in basis points.
    pub pss_to_full_copy_basis_points: Option<u32>,
}

/// Whether a fault result came from a real injection API or a symbolic boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A fault was injected through a real implementation hook.
    Actual,
    /// The required integration API does not exist; no behavior was fabricated.
    Symbolic,
}

/// One checkpoint/effect-stage fault row.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FaultRow {
    /// Stable stage name.
    pub stage: String,
    /// Evidence source.
    pub evidence_kind: EvidenceKind,
    /// Explicit result.
    pub outcome: SampleOutcome,
    /// Whether cleanup/atomicity containment was verified.
    pub containment_verified: bool,
    /// Always false unless a downstream reconciliation API proves otherwise.
    pub claims_external_exactly_once: bool,
    /// Bounded supporting facts.
    pub evidence: BTreeMap<String, String>,
}

/// Complete fault matrix.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FaultMatrix {
    /// Every actual and symbolic stage.
    pub rows: Vec<FaultRow>,
}

/// Recommendation classification.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Evidence supports retaining the mechanism.
    Keep,
    /// Evidence supports a narrower compatibility or scope claim.
    Narrow,
    /// Evidence rejects the proposed claim or architecture.
    Kill,
}

/// One threshold-backed decision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DecisionEvidence {
    /// Mechanism or claim.
    pub mechanism: String,
    /// Recommendation.
    pub decision: Decision,
    /// Predeclared threshold or correctness rule.
    pub threshold: String,
    /// Measured facts used for the decision.
    pub evidence: Vec<String>,
}

/// Stable Week 4 evidence bundle.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EvidenceBundle {
    /// Artifact schema version.
    pub schema_version: u32,
    /// Unique run identifier.
    pub run_id: String,
    /// Requested suite.
    pub suite: SuiteName,
    /// Validated environment.
    pub environment: BenchmarkEnvironment,
    /// Frozen thresholds.
    pub thresholds: DecisionThresholds,
    /// Checkpoint suite when requested.
    pub checkpoint: Option<CheckpointSuiteEvidence>,
    /// Compatibility matrix when requested.
    pub compatibility: Option<CompatibilityMatrix>,
    /// COW suite when requested.
    pub cow: Option<CowEvidence>,
    /// Fault matrix when requested.
    pub faults: Option<FaultMatrix>,
    /// Derived keep/narrow/kill decisions.
    pub decisions: Vec<DecisionEvidence>,
}

#[derive(Clone, Copy)]
struct CsvRow<'a> {
    section: &'a str,
    case: &'a str,
    status: &'a str,
    elapsed_ns: u64,
    message: &'a str,
    evidence: &'a str,
}

impl EvidenceBundle {
    /// Stable pretty JSON.
    ///
    /// # Errors
    ///
    /// Returns a serialization error for non-finite metrics.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Stable CSV retaining successful, unsupported, failed, and symbolic rows.
    ///
    /// # Errors
    ///
    /// Returns a serialization error for non-finite metrics.
    pub fn to_csv(&self) -> serde_json::Result<String> {
        let mut output = String::from(
            "schema_version,run_id,section,case,status,elapsed_ns,message,evidence_json\n",
        );
        append_checkpoint_csv(&mut output, self)?;
        append_compatibility_csv(&mut output, self)?;
        append_cow_csv(&mut output, self)?;
        append_fault_csv(&mut output, self)?;
        Ok(output)
    }

    /// Concise evidence-backed Markdown recommendation.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut output = format!(
            "# Epoch benchmark report\n\nRun `{}` used revision `{}`{} on {} {} ({}, {} CPUs).\n\n",
            self.run_id,
            self.environment.code_revision,
            if self.environment.code_dirty {
                " (dirty)"
            } else {
                ""
            },
            self.environment.os,
            self.environment.architecture,
            self.environment.cpu_model,
            self.environment.cpu_count,
        );
        output.push_str("## Predeclared thresholds\n\n");
        let _ = writeln!(
            output,
            "- checkpoint capture p95: at most {} ns\n- checkpoint restore p95: at most {} ns\n- checkpoint validation failures: at most {}\n- COW PSS/full-copy ratio: at most {} basis points\n",
            self.thresholds.checkpoint_capture_p95_ns,
            self.thresholds.checkpoint_restore_p95_ns,
            self.thresholds.checkpoint_validation_failures,
            self.thresholds.cow_pss_ratio_basis_points,
        );
        output.push_str(
            "## Keep / narrow / kill\n\n| Decision | Mechanism | Evidence |\n|---|---|---|\n",
        );
        for decision in &self.decisions {
            let evidence = decision.evidence.join("; ");
            let _ = writeln!(
                output,
                "| {:?} | {} | {} |",
                decision.decision, decision.mechanism, evidence
            );
        }
        output.push_str(
            "\nUnsupported and failed rows remain in the JSON and CSV artifacts; no external exactly-once guarantee is inferred.\n",
        );
        output
    }
}

fn append_checkpoint_csv(output: &mut String, bundle: &EvidenceBundle) -> serde_json::Result<()> {
    let Some(checkpoint) = &bundle.checkpoint else {
        return Ok(());
    };
    for report in &checkpoint.reports {
        for sample in &report.samples {
            csv_row(
                output,
                bundle,
                CsvRow {
                    section: "checkpoint",
                    case: &format!("{}-{}", report.config.trace_mode.label(), sample.ordinal),
                    status: sample.outcome.label(),
                    elapsed_ns: sample.elapsed_ns,
                    message: sample.outcome.message(),
                    evidence: &serde_json::to_string(&sample.metrics)?,
                },
            );
        }
    }
    for case in &checkpoint.validation_cases {
        csv_row(
            output,
            bundle,
            CsvRow {
                section: "validation",
                case: &case.name,
                status: if case.passed { "succeeded" } else { "failed" },
                elapsed_ns: 0,
                message: &case.detail,
                evidence: "{}",
            },
        );
    }
    Ok(())
}

fn append_compatibility_csv(
    output: &mut String,
    bundle: &EvidenceBundle,
) -> serde_json::Result<()> {
    let Some(matrix) = &bundle.compatibility else {
        return Ok(());
    };
    for row in &matrix.rows {
        csv_row(
            output,
            bundle,
            CsvRow {
                section: "compatibility",
                case: &row.case,
                status: row.outcome.label(),
                elapsed_ns: row.elapsed_ns,
                message: row.outcome.message(),
                evidence: &serde_json::to_string(&row.evidence)?,
            },
        );
    }
    Ok(())
}

fn append_cow_csv(output: &mut String, bundle: &EvidenceBundle) -> serde_json::Result<()> {
    let Some(cow) = &bundle.cow else {
        return Ok(());
    };
    if cow.samples.is_empty() {
        csv_row(
            output,
            bundle,
            CsvRow {
                section: "cow",
                case: "experiment",
                status: cow.outcome.label(),
                elapsed_ns: 0,
                message: cow.outcome.message(),
                evidence: "{}",
            },
        );
    } else {
        for sample in &cow.samples {
            csv_row(
                output,
                bundle,
                CsvRow {
                    section: "cow",
                    case: &format!("sample-{}", sample.ordinal),
                    status: "succeeded",
                    elapsed_ns: sample.elapsed_ns,
                    message: "",
                    evidence: &serde_json::to_string(sample)?,
                },
            );
        }
    }
    Ok(())
}

fn append_fault_csv(output: &mut String, bundle: &EvidenceBundle) -> serde_json::Result<()> {
    let Some(faults) = &bundle.faults else {
        return Ok(());
    };
    for row in &faults.rows {
        csv_row(
            output,
            bundle,
            CsvRow {
                section: "fault",
                case: &row.stage,
                status: row.outcome.label(),
                elapsed_ns: 0,
                message: row.outcome.message(),
                evidence: &serde_json::to_string(&row.evidence)?,
            },
        );
    }
    Ok(())
}

/// Suite execution failure.
#[derive(Debug, Error)]
pub enum SuiteError {
    /// Resource or path configuration was rejected before execution.
    #[error("invalid benchmark configuration: {0}")]
    InvalidConfig(String),
    /// Local filesystem setup or persistence failed.
    #[error("benchmark I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A required Week 2 API unexpectedly failed during a supported benchmark case.
    #[error("benchmark backend failed: {0}")]
    Backend(String),
    /// Helper output was malformed.
    #[error("benchmark helper output was invalid: {0}")]
    Helper(String),
}

/// Runs the requested suite and derives recommendations from frozen thresholds.
///
/// # Errors
///
/// Returns a configuration, setup, or required supported-backend failure.
pub fn run_suite(
    request: &SuiteRequest,
    environment: BenchmarkEnvironment,
) -> Result<EvidenceBundle, SuiteError> {
    request.checkpoint.validate()?;
    request.cow.validate()?;
    let checkpoint = matches!(request.suite, SuiteName::Checkpoint | SuiteName::All)
        .then(|| run_checkpoint_suite(&request.checkpoint, &environment))
        .transpose()?;
    let compatibility = matches!(request.suite, SuiteName::Compatibility | SuiteName::All)
        .then(|| run_compatibility_matrix(&request.checkpoint, environment.clone()))
        .transpose()?;
    let cow = matches!(request.suite, SuiteName::Cow | SuiteName::All)
        .then(|| run_cow_experiment(&request.cow, environment.clone()));
    let faults = matches!(request.suite, SuiteName::Faults | SuiteName::All)
        .then(|| run_fault_matrix(&request.checkpoint.root.join("faults")))
        .transpose()?;
    let decisions = decisions(
        checkpoint.as_ref(),
        cow.as_ref(),
        faults.as_ref(),
        &request.thresholds,
    );
    Ok(EvidenceBundle {
        schema_version: 1,
        run_id: format!("bench-{}", Uuid::new_v4()),
        suite: request.suite,
        environment,
        thresholds: request.thresholds.clone(),
        checkpoint,
        compatibility,
        cow,
        faults,
        decisions,
    })
}

fn decisions(
    checkpoint: Option<&CheckpointSuiteEvidence>,
    cow: Option<&CowEvidence>,
    faults: Option<&FaultMatrix>,
    thresholds: &DecisionThresholds,
) -> Vec<DecisionEvidence> {
    let checkpoint_measurement = checkpoint.map(checkpoint_decision_measurement);
    let checkpoint_evidence = checkpoint_measurement.as_ref().map_or_else(
        || vec!["checkpoint suite was not requested".to_owned()],
        |measurement| {
            vec![
                format!("successful_samples={}", measurement.successes),
                format!("validation_failures={}", measurement.validation_failures),
                format!(
                    "capture_p95_ns={}",
                    optional_metric(measurement.capture_p95_ns)
                ),
                format!(
                    "restore_p95_ns={}",
                    optional_metric(measurement.restore_p95_ns)
                ),
            ]
        },
    );
    let checkpoint_keep = checkpoint_measurement.is_some_and(|measurement| {
        measurement.failed == 0
            && measurement.unsupported == 0
            && measurement.validation_failures <= thresholds.checkpoint_validation_failures
            && measurement
                .capture_p95_ns
                .is_some_and(|value| value <= thresholds.checkpoint_capture_p95_ns)
            && measurement
                .restore_p95_ns
                .is_some_and(|value| value <= thresholds.checkpoint_restore_p95_ns)
    });
    let cow_evidence = cow.map_or_else(
        || vec!["COW suite was not requested".to_owned()],
        |evidence| match &evidence.outcome {
            SampleOutcome::Succeeded => vec![format!(
                "{} Linux raw samples; pss_to_full_copy_basis_points={}; scope remains process-memory compatibility only",
                evidence.samples.len(),
                evidence
                    .summary
                    .as_ref()
                    .and_then(|summary| summary.pss_to_full_copy_basis_points)
                    .map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
            )],
            SampleOutcome::Unsupported { reason } => vec![format!("unsupported: {reason}")],
            SampleOutcome::Failed { error } => vec![format!("failed: {error}")],
        },
    );
    vec![
        DecisionEvidence {
            mechanism: "cooperative application + full-copy workspace checkpoint".to_owned(),
            decision: if checkpoint_keep {
                Decision::Keep
            } else {
                Decision::Narrow
            },
            threshold: format!(
                "zero unsupported/failed samples and <= {} validation failures",
                thresholds.checkpoint_validation_failures
            ),
            evidence: checkpoint_evidence,
        },
        DecisionEvidence {
            mechanism: "fork COW process-memory optimization".to_owned(),
            decision: Decision::Narrow,
            threshold: format!(
                "PSS/full-copy <= {} bp and Linux compatibility only",
                thresholds.cow_pss_ratio_basis_points
            ),
            evidence: cow_evidence,
        },
        effect_decision(faults),
        DecisionEvidence {
            mechanism: "transparent external exactly-once through rollback".to_owned(),
            decision: Decision::Kill,
            threshold: "requires a downstream idempotency/reconciliation API and crash evidence"
                .to_owned(),
            evidence: vec![
                "the integrated gateway proves durable local duplicate suppression, not a live provider's commit semantics".to_owned(),
                "live provider reconciliation remains an explicit unsupported matrix row".to_owned(),
            ],
        },
    ]
}

fn effect_decision(faults: Option<&FaultMatrix>) -> DecisionEvidence {
    const REQUIRED_STAGES: [&str; 4] = [
        "effect_replay_100_runs",
        "effect_unknown_suspends_branch",
        "capability_revocation_resurrection_blocked",
        "capability_policy_rollback_blocked",
    ];
    let keep = faults.is_some_and(|matrix| {
        REQUIRED_STAGES.into_iter().all(|stage| {
            matrix
                .rows
                .iter()
                .find(|row| row.stage == stage)
                .is_some_and(|row| {
                    row.evidence_kind == EvidenceKind::Actual
                        && matches!(row.outcome, SampleOutcome::Succeeded)
                        && row.containment_verified
                })
        })
    });
    let evidence = faults.map_or_else(
        || vec!["fault suite was not requested".to_owned()],
        |matrix| {
            REQUIRED_STAGES
                .iter()
                .map(|stage| {
                    let status = matrix
                        .rows
                        .iter()
                        .find(|row| row.stage == *stage)
                        .map_or("missing", |row| row.outcome.label());
                    format!("{stage}={status}")
                })
                .collect()
        },
    );
    DecisionEvidence {
        mechanism: "durable effect gateway replay and fail-closed authority".to_owned(),
        decision: if keep { Decision::Keep } else { Decision::Narrow },
        threshold: "100 stable replays cause one deterministic dispatch; unknown outcomes suspend; revoked/stale authority stays denied".to_owned(),
        evidence,
    }
}

struct CheckpointDecisionMeasurement {
    successes: u32,
    unsupported: u32,
    failed: u32,
    validation_failures: u32,
    capture_p95_ns: Option<u64>,
    restore_p95_ns: Option<u64>,
}

fn checkpoint_decision_measurement(
    evidence: &CheckpointSuiteEvidence,
) -> CheckpointDecisionMeasurement {
    let mut capture = Vec::new();
    let mut restore = Vec::new();
    for report in &evidence.reports {
        for sample in &report.samples {
            if !matches!(sample.outcome, SampleOutcome::Succeeded) {
                continue;
            }
            let capture_ns =
                metric_u64(&sample.metrics, "application_capture_ns").and_then(|value| {
                    metric_u64(&sample.metrics, "workspace_capture_ns")
                        .and_then(|workspace| value.checked_add(workspace))
                });
            let restore_ns = [
                "application_restore_ns",
                "workspace_validation_ns",
                "workspace_restore_ns",
            ]
            .into_iter()
            .try_fold(0_u64, |total, key| {
                metric_u64(&sample.metrics, key).and_then(|value| total.checked_add(value))
            });
            if let Some(value) = capture_ns {
                capture.push(value);
            }
            if let Some(value) = restore_ns {
                restore.push(value);
            }
        }
    }
    capture.sort_unstable();
    restore.sort_unstable();
    CheckpointDecisionMeasurement {
        successes: evidence
            .reports
            .iter()
            .map(|report| report.summary.succeeded)
            .sum(),
        unsupported: evidence
            .reports
            .iter()
            .map(|report| report.summary.unsupported)
            .sum(),
        failed: evidence
            .reports
            .iter()
            .map(|report| report.summary.failed)
            .sum(),
        validation_failures: u32::try_from(
            evidence
                .validation_cases
                .iter()
                .filter(|case| !case.passed)
                .count(),
        )
        .unwrap_or(u32::MAX),
        capture_p95_ns: percentile(&capture, 95),
        restore_p95_ns: percentile(&restore, 95),
    }
}

fn metric_u64(metrics: &BTreeMap<String, f64>, key: &str) -> Option<u64> {
    let value = metrics.get(key)?;
    if !value.is_finite() || *value < 0.0 {
        return None;
    }
    format!("{value:.0}").parse().ok()
}

fn percentile(values: &[u64], percentile: usize) -> Option<u64> {
    let rank = percentile.saturating_mul(values.len()).div_ceil(100).max(1);
    values.get(rank - 1).copied()
}

fn optional_metric(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
}

fn csv_row(output: &mut String, report: &EvidenceBundle, row: CsvRow<'_>) {
    let _ = writeln!(
        output,
        "{},{},{},{},{},{},{},{}",
        report.schema_version,
        csv_field(&report.run_id),
        csv_field(row.section),
        csv_field(row.case),
        row.status,
        row.elapsed_ns,
        csv_field(row.message),
        csv_field(row.evidence),
    );
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

pub(super) fn bounded(value: &str) -> String {
    const LIMIT: usize = 4_096;
    let mut end = value.len().min(LIMIT);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}
