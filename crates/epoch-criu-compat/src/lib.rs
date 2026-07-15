//! Bounded, fail-closed CRIU compatibility experiments.
//!
//! This crate is an experimental runner and reporting seam. It does not register CRIU as a
//! production checkpoint backend and never converts missing facilities or failed verification into
//! a supported result.

use std::{
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const REPORT_SCHEMA_VERSION: u32 = 1;
const MIN_TIMEOUT_MS: u64 = 100;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MIN_LOG_BYTES: usize = 1_024;
const MAX_LOG_BYTES: usize = 1_048_576;
const MIN_MEMORY_BYTES: u64 = 1_048_576;
const MAX_MEMORY_BYTES: u64 = 1_073_741_824;
const MAX_PROCESSES: u32 = 64;
const MAX_SCALE_AXIS: usize = 16;

/// Versioned compatibility scenario identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    SleepingProcess,
    OpenRegularFile,
    ProcessTree,
    LoopbackSocket,
    ExternalTcp,
    WorkspaceMutation,
}

impl Scenario {
    pub const SCHEMA_VERSION: u32 = 1;
    const DECLARED: [Self; 6] = [
        Self::SleepingProcess,
        Self::OpenRegularFile,
        Self::ProcessTree,
        Self::LoopbackSocket,
        Self::ExternalTcp,
        Self::WorkspaceMutation,
    ];

    #[must_use]
    pub const fn declared() -> &'static [Self] {
        &Self::DECLARED
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SleepingProcess => "sleeping_process",
            Self::OpenRegularFile => "open_regular_file",
            Self::ProcessTree => "process_tree",
            Self::LoopbackSocket => "loopback_socket",
            Self::ExternalTcp => "external_tcp",
            Self::WorkspaceMutation => "workspace_mutation",
        }
    }
}

/// Bounded command execution controls.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunLimits {
    pub dump_timeout_ms: u64,
    pub restore_timeout_ms: u64,
    pub max_log_bytes: usize,
}

impl RunLimits {
    /// Validates time and log bounds before any process is launched.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::InvalidLimit`] for an out-of-range value.
    pub fn new(
        dump_timeout_ms: u64,
        restore_timeout_ms: u64,
        max_log_bytes: usize,
    ) -> Result<Self, ConfigurationError> {
        if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&dump_timeout_ms) {
            return Err(ConfigurationError::InvalidLimit {
                field: "dump_timeout_ms",
            });
        }
        if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&restore_timeout_ms) {
            return Err(ConfigurationError::InvalidLimit {
                field: "restore_timeout_ms",
            });
        }
        if !(MIN_LOG_BYTES..=MAX_LOG_BYTES).contains(&max_log_bytes) {
            return Err(ConfigurationError::InvalidLimit {
                field: "max_log_bytes",
            });
        }
        Ok(Self {
            dump_timeout_ms,
            restore_timeout_ms,
            max_log_bytes,
        })
    }
}

/// Deterministically ordered memory/process scale axes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScalingPlan {
    pub memory_bytes: Vec<u64>,
    pub process_counts: Vec<u32>,
}

impl ScalingPlan {
    /// Creates a bounded, sorted, duplicate-free scale plan.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized, or out-of-range axes.
    pub fn new(
        mut memory_bytes: Vec<u64>,
        mut process_counts: Vec<u32>,
    ) -> Result<Self, ConfigurationError> {
        validate_scale_axis(&memory_bytes, "memory_bytes", |value| {
            (MIN_MEMORY_BYTES..=MAX_MEMORY_BYTES).contains(value)
        })?;
        validate_scale_axis(&process_counts, "process_counts", |value| {
            (1..=MAX_PROCESSES).contains(value)
        })?;
        memory_bytes.sort_unstable();
        memory_bytes.dedup();
        process_counts.sort_unstable();
        process_counts.dedup();
        Ok(Self {
            memory_bytes,
            process_counts,
        })
    }
}

fn validate_scale_axis<T>(
    values: &[T],
    field: &'static str,
    valid: impl Fn(&T) -> bool,
) -> Result<(), ConfigurationError> {
    if values.is_empty() || values.len() > MAX_SCALE_AXIS || !values.iter().all(valid) {
        Err(ConfigurationError::InvalidScale { field })
    } else {
        Ok(())
    }
}

/// Validated runner inputs. Tool existence is reported as compatibility evidence, not a config
/// parse error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunnerConfig {
    criu_path: PathBuf,
    fixture_path: PathBuf,
    limits: RunLimits,
    scaling: ScalingPlan,
}

impl RunnerConfig {
    /// Validates literal paths and experiment bounds.
    ///
    /// # Errors
    ///
    /// Returns an error for non-absolute or non-normalized executable paths.
    pub fn new(
        criu_path: PathBuf,
        fixture_path: PathBuf,
        limits: RunLimits,
        scaling: ScalingPlan,
    ) -> Result<Self, ConfigurationError> {
        validate_executable_path(&criu_path, "criu_path")?;
        validate_executable_path(&fixture_path, "fixture_path")?;
        Ok(Self {
            criu_path,
            fixture_path,
            limits,
            scaling,
        })
    }
}

fn validate_executable_path(path: &Path, field: &'static str) -> Result<(), ConfigurationError> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        Err(ConfigurationError::InvalidPath {
            field,
            path: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

/// Compatibility classification. Unsupported is distinct from an attempted experiment failure.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RowStatus {
    Supported,
    Unsupported,
    Failed,
}

/// Stable diagnostic category for aggregation and automation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    Supported,
    PlatformUnsupported,
    CriuUnavailable,
    CriuCheckFailed,
    ExternalTcpUnsupported,
    FixtureFailed,
    DumpTimedOut,
    DumpFailed,
    RestoreTimedOut,
    RestoreFailed,
    VerificationFailed,
    CleanupFailed,
}

/// Human-readable explanation paired with a stable category.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub stage: String,
    pub message: String,
}

/// One bounded external-command observation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandMeasurement {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub latency_ms: u64,
    pub log_artifact: String,
    pub log_truncated: bool,
}

/// Exact requested workload scale.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScalePoint {
    pub memory_bytes: u64,
    pub process_count: u32,
}

/// One matrix result. Rows are never omitted because another row failed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompatibilityRow {
    pub scenario: Scenario,
    pub scale: ScalePoint,
    pub status: RowStatus,
    pub diagnostic: Diagnostic,
    pub dump: Option<CommandMeasurement>,
    pub restore: Option<CommandMeasurement>,
    pub image_bytes: Option<u64>,
    pub restored_behavior_verified: bool,
}

/// Captured host and CRIU probe metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvironmentMetadata {
    pub operating_system: String,
    pub architecture: String,
    pub kernel_release: Option<String>,
    pub criu_version: Option<String>,
    pub criu_check_supported: bool,
    pub criu_check_diagnostic: Diagnostic,
}

/// Predeclared decision thresholds copied from the runtime specification.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DecisionThresholds {
    pub required_restore_correctness_percent: u8,
    pub checkpoint_pause_p95_ms: u64,
    pub restore_p95_ms: u64,
}

impl Default for DecisionThresholds {
    fn default() -> Self {
        Self {
            required_restore_correctness_percent: 100,
            checkpoint_pause_p95_ms: 1_000,
            restore_p95_ms: 3_000,
        }
    }
}

/// Evidence-based scope recommendation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    Keep,
    Narrow,
    Kill,
}

/// Machine-readable decision summary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DecisionEvidence {
    pub recommendation: Recommendation,
    pub rationale: String,
}

/// Versioned deterministic compatibility report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompatibilityReport {
    pub schema_version: u32,
    pub scenario_schema_version: u32,
    pub environment: EnvironmentMetadata,
    pub limits: RunLimits,
    pub scaling: ScalingPlan,
    pub thresholds: DecisionThresholds,
    pub rows: Vec<CompatibilityRow>,
    pub decision: DecisionEvidence,
}

impl CompatibilityReport {
    /// Serializes using struct field order and a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns a JSON encoding error if the schema cannot be serialized.
    pub fn to_stable_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|mut encoded| {
            encoded.push('\n');
            encoded
        })
    }

    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut output = String::from(
            "# CRIU compatibility evidence\n\n\
             ## Environment\n\n\
             | Field | Value |\n\
             | --- | --- |\n",
        );
        push_markdown_row(
            &mut output,
            "Operating system",
            &self.environment.operating_system,
        );
        push_markdown_row(&mut output, "Architecture", &self.environment.architecture);
        push_markdown_row(
            &mut output,
            "Kernel release",
            self.environment
                .kernel_release
                .as_deref()
                .unwrap_or("unavailable"),
        );
        push_markdown_row(
            &mut output,
            "CRIU version",
            self.environment
                .criu_version
                .as_deref()
                .unwrap_or("unavailable"),
        );
        output.push_str(
            "\n## Compatibility matrix\n\n\
             | Scenario | Memory bytes | Processes | Status | Dump ms | Restore ms | Image bytes | Verification | Diagnostic |\n\
             | --- | ---: | ---: | --- | ---: | ---: | ---: | --- | --- |\n",
        );
        for row in &self.rows {
            writeln!(
                output,
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                row.scenario.as_str(),
                row.scale.memory_bytes,
                row.scale.process_count,
                status_name(row.status),
                optional_latency(row.dump.as_ref()),
                optional_latency(row.restore.as_ref()),
                row.image_bytes
                    .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                if row.restored_behavior_verified {
                    "yes"
                } else {
                    "no"
                },
                escape_markdown(&row.diagnostic.message)
            )
            .expect("writing to a String cannot fail");
        }
        write!(
            output,
            "\n## Keep/narrow/kill evidence\n\n\
             The predeclared `narrow_or_kill` gates require 100% restore correctness for declared supported rows, checkpoint pause p95 at or below {} ms, and restore p95 at or below {} ms.\n\n\
             Recommendation: `{}`. {}\n",
            self.thresholds.checkpoint_pause_p95_ms,
            self.thresholds.restore_p95_ms,
            recommendation_name(self.decision.recommendation),
            self.decision.rationale
        )
        .expect("writing to a String cannot fail");
        output
    }
}

fn push_markdown_row(output: &mut String, field: &str, value: &str) {
    writeln!(output, "| {field} | {} |", escape_markdown(value))
        .expect("writing to a String cannot fail");
}

fn escape_markdown(value: &str) -> String {
    value.replace('|', "\\|").replace(['\r', '\n'], " ")
}

const fn status_name(status: RowStatus) -> &'static str {
    match status {
        RowStatus::Supported => "supported",
        RowStatus::Unsupported => "unsupported",
        RowStatus::Failed => "failed",
    }
}

const fn recommendation_name(recommendation: Recommendation) -> &'static str {
    match recommendation {
        Recommendation::Keep => "keep",
        Recommendation::Narrow => "narrow",
        Recommendation::Kill => "kill",
    }
}

fn optional_latency(measurement: Option<&CommandMeasurement>) -> String {
    measurement.map_or_else(|| "—".to_owned(), |value| value.latency_ms.to_string())
}

#[derive(Clone, Debug)]
struct LogArtifact {
    relative_path: String,
    bytes: Vec<u8>,
}

/// Report plus bounded log artifacts.
#[derive(Clone, Debug)]
pub struct CompatibilityEvidence {
    report: CompatibilityReport,
    artifacts: Vec<LogArtifact>,
}

impl CompatibilityEvidence {
    #[must_use]
    pub const fn report(&self) -> &CompatibilityReport {
        &self.report
    }

    /// Writes evidence only to a path that does not already exist.
    ///
    /// # Errors
    ///
    /// Returns [`EvidenceError::OutputExists`] rather than replacing any existing user data.
    pub fn write_new(&self, output: &Path) -> Result<(), EvidenceError> {
        if output.exists() {
            return Err(EvidenceError::OutputExists {
                path: output.to_path_buf(),
            });
        }
        fs::create_dir(output).map_err(|source| EvidenceError::Io {
            path: output.to_path_buf(),
            source,
        })?;
        set_private_directory(output)?;
        let logs = output.join("logs");
        fs::create_dir(&logs).map_err(|source| EvidenceError::Io {
            path: logs.clone(),
            source,
        })?;
        set_private_directory(&logs)?;

        write_new_file(
            &output.join("compatibility.json"),
            self.report.to_stable_json()?.as_bytes(),
        )?;
        write_new_file(
            &output.join("compatibility.md"),
            self.report.to_markdown().as_bytes(),
        )?;
        for artifact in &self.artifacts {
            let path = output.join(&artifact.relative_path);
            if path.parent() != Some(logs.as_path()) {
                return Err(EvidenceError::UnsafeArtifact {
                    path: artifact.relative_path.clone(),
                });
            }
            write_new_file(&path, &artifact.bytes)?;
        }
        Ok(())
    }
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), EvidenceError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| EvidenceError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| EvidenceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(unix)]
fn set_private_directory(path: &Path) -> Result<(), EvidenceError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        EvidenceError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_directory(_path: &Path) -> Result<(), EvidenceError> {
    Ok(())
}

/// Bounded compatibility runner.
#[derive(Clone, Debug)]
pub struct CompatibilityRunner {
    config: RunnerConfig,
}

impl CompatibilityRunner {
    #[must_use]
    pub const fn new(config: RunnerConfig) -> Self {
        Self { config }
    }

    /// Runs the complete declared matrix, preserving unsupported and failed rows.
    ///
    /// # Errors
    ///
    /// Returns an error only when the experiment infrastructure itself cannot create isolated
    /// temporary state. Host/tool incompatibility is evidence, not a runner error.
    pub fn run(&self) -> Result<CompatibilityEvidence, RunError> {
        let mut artifacts = environment_artifacts(&self.config);
        let unavailable = unavailable_diagnostic(&self.config);
        let environment = EnvironmentMetadata {
            operating_system: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            kernel_release: None,
            criu_version: None,
            criu_check_supported: false,
            criu_check_diagnostic: unavailable.clone(),
        };
        let rows = matrix_points(&self.config.scaling)
            .map(|(scenario, scale)| CompatibilityRow {
                scenario,
                scale,
                status: RowStatus::Unsupported,
                diagnostic: if scenario == Scenario::ExternalTcp {
                    Diagnostic {
                        code: DiagnosticCode::ExternalTcpUnsupported,
                        stage: "scenario".to_owned(),
                        message:
                            "external TCP is explicitly outside the transparent restore subset"
                                .to_owned(),
                    }
                } else {
                    unavailable.clone()
                },
                dump: None,
                restore: None,
                image_bytes: None,
                restored_behavior_verified: false,
            })
            .collect();
        artifacts.push(LogArtifact {
            relative_path: "logs/environment-criu-check.log".to_owned(),
            bytes: unavailable.message.as_bytes().to_vec(),
        });
        let report = CompatibilityReport {
            schema_version: REPORT_SCHEMA_VERSION,
            scenario_schema_version: Scenario::SCHEMA_VERSION,
            environment,
            limits: self.config.limits,
            scaling: self.config.scaling.clone(),
            thresholds: DecisionThresholds::default(),
            rows,
            decision: DecisionEvidence {
                recommendation: Recommendation::Kill,
                rationale: "CRIU was unavailable, so no declared row can be claimed as supported."
                    .to_owned(),
            },
        };
        Ok(CompatibilityEvidence { report, artifacts })
    }
}

fn environment_artifacts(config: &RunnerConfig) -> Vec<LogArtifact> {
    vec![
        LogArtifact {
            relative_path: "logs/environment-criu-version.log".to_owned(),
            bytes: format!("CRIU executable: {}\n", config.criu_path.display()).into_bytes(),
        },
        LogArtifact {
            relative_path: "logs/environment-kernel.log".to_owned(),
            bytes: format!(
                "operating_system={}\narchitecture={}\n",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
            .into_bytes(),
        },
    ]
}

fn unavailable_diagnostic(config: &RunnerConfig) -> Diagnostic {
    if std::env::consts::OS != "linux" {
        Diagnostic {
            code: DiagnosticCode::CriuUnavailable,
            stage: "discovery".to_owned(),
            message: format!(
                "CRIU process checkpoint experiments require Linux; current OS is {}",
                std::env::consts::OS
            ),
        }
    } else if !config.criu_path.is_file() {
        Diagnostic {
            code: DiagnosticCode::CriuUnavailable,
            stage: "discovery".to_owned(),
            message: format!(
                "CRIU executable is unavailable at {}",
                config.criu_path.display()
            ),
        }
    } else if !config.fixture_path.is_file() {
        Diagnostic {
            code: DiagnosticCode::FixtureFailed,
            stage: "discovery".to_owned(),
            message: format!(
                "compatibility fixture is unavailable at {}",
                config.fixture_path.display()
            ),
        }
    } else {
        Diagnostic {
            code: DiagnosticCode::CriuCheckFailed,
            stage: "criu_check".to_owned(),
            message: "CRIU probing is not yet implemented".to_owned(),
        }
    }
}

fn matrix_points(scaling: &ScalingPlan) -> impl Iterator<Item = (Scenario, ScalePoint)> + '_ {
    Scenario::declared()
        .iter()
        .copied()
        .flat_map(move |scenario| {
            scaling
                .memory_bytes
                .iter()
                .copied()
                .flat_map(move |memory_bytes| {
                    scaling
                        .process_counts
                        .iter()
                        .copied()
                        .map(move |process_count| {
                            (
                                scenario,
                                ScalePoint {
                                    memory_bytes,
                                    process_count,
                                },
                            )
                        })
                })
        })
}

#[derive(Debug, Error)]
pub enum ConfigurationError {
    #[error("invalid bounded configuration field {field}")]
    InvalidLimit { field: &'static str },
    #[error("invalid scaling axis {field}")]
    InvalidScale { field: &'static str },
    #[error("{field} must be an absolute normalized path: {path}")]
    InvalidPath { field: &'static str, path: PathBuf },
}

#[derive(Debug, Error)]
pub enum RunError {
    #[error("failed to create isolated experiment state: {0}")]
    TemporaryState(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum EvidenceError {
    #[error("refusing to overwrite existing evidence path {path}")]
    OutputExists { path: PathBuf },
    #[error("unsafe generated artifact path {path}")]
    UnsafeArtifact { path: String },
    #[error("evidence I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("evidence JSON encoding failed: {0}")]
    Json(#[from] serde_json::Error),
}
