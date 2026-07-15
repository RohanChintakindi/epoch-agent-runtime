//! Bounded, fail-closed CRIU compatibility experiments.
//!
//! This crate is an experimental runner and reporting seam. It does not register CRIU as a
//! production checkpoint backend and never converts missing facilities or failed verification into
//! a supported result.

use std::{
    ffi::OsString,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde::{Deserialize, Serialize};
use tempfile::{Builder as TempBuilder, TempDir};
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
const MAX_SCALE_POINTS: usize = 16;

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
            (2..=MAX_PROCESSES).contains(value)
        })?;
        if memory_bytes
            .len()
            .checked_mul(process_counts.len())
            .is_none_or(|points| points > MAX_SCALE_POINTS)
        {
            return Err(ConfigurationError::InvalidScale {
                field: "matrix_scale_points",
            });
        }
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
    DumpUnsupported,
    DumpFailed,
    RestoreTimedOut,
    RestoreUnsupported,
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
    pub log_artifact: String,
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
    pub criu_check_all_supported: bool,
    pub criu_check_all_diagnostic: Diagnostic,
    pub effective_uid: Option<u32>,
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
        push_markdown_row(
            &mut output,
            "CRIU basic check",
            if self.environment.criu_check_supported {
                "supported"
            } else {
                "unsupported"
            },
        );
        push_markdown_row(
            &mut output,
            "CRIU extended check",
            if self.environment.criu_check_all_supported {
                "supported"
            } else {
                "warnings or unsupported"
            },
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
        let temporary = TempBuilder::new().prefix("epoch-criu-").tempdir()?;
        set_private_run_directory(temporary.path())?;
        let mut artifacts = Vec::new();
        let probe = self.probe_environment(&temporary, &mut artifacts)?;
        let rows = if probe.environment.criu_check_supported {
            self.run_supported_matrix(&temporary, &mut artifacts)?
        } else {
            unsupported_rows(&self.config.scaling, &probe.diagnostic, &mut artifacts)
        };
        let decision = decide(&rows, DecisionThresholds::default());
        Ok(CompatibilityEvidence {
            report: CompatibilityReport {
                schema_version: REPORT_SCHEMA_VERSION,
                scenario_schema_version: Scenario::SCHEMA_VERSION,
                environment: probe.environment,
                limits: self.config.limits,
                scaling: self.config.scaling.clone(),
                thresholds: DecisionThresholds::default(),
                rows,
                decision,
            },
            artifacts,
        })
    }

    fn probe_environment(
        &self,
        temporary: &TempDir,
        artifacts: &mut Vec<LogArtifact>,
    ) -> Result<EnvironmentProbe, RunError> {
        let kernel = run_bounded(
            Path::new("/usr/bin/uname"),
            &[OsString::from("-r")],
            temporary.path(),
            self.config.limits.dump_timeout_ms,
            self.config.limits.max_log_bytes,
            "logs/environment-kernel.log",
        )?;
        let kernel_release = successful_first_line(&kernel);
        artifacts.push(kernel.artifact());

        let unavailable = unavailable_diagnostic(&self.config);
        if unavailable.code != DiagnosticCode::CriuCheckFailed {
            artifacts.push(LogArtifact {
                relative_path: "logs/environment-criu-version.log".to_owned(),
                bytes: unavailable.message.as_bytes().to_vec(),
            });
            artifacts.push(LogArtifact {
                relative_path: unavailable.log_artifact.clone(),
                bytes: unavailable.message.as_bytes().to_vec(),
            });
            artifacts.push(LogArtifact {
                relative_path: "logs/environment-criu-check-all.log".to_owned(),
                bytes: unavailable.message.as_bytes().to_vec(),
            });
            return Ok(EnvironmentProbe {
                environment: EnvironmentMetadata {
                    operating_system: std::env::consts::OS.to_owned(),
                    architecture: std::env::consts::ARCH.to_owned(),
                    kernel_release,
                    criu_version: None,
                    criu_check_supported: false,
                    criu_check_diagnostic: unavailable.clone(),
                    criu_check_all_supported: false,
                    criu_check_all_diagnostic: Diagnostic {
                        log_artifact: "logs/environment-criu-check-all.log".to_owned(),
                        ..unavailable.clone()
                    },
                    effective_uid: effective_uid(),
                },
                diagnostic: unavailable,
            });
        }

        let criu = self.probe_available_criu(temporary, artifacts)?;
        Ok(EnvironmentProbe {
            environment: EnvironmentMetadata {
                operating_system: std::env::consts::OS.to_owned(),
                architecture: std::env::consts::ARCH.to_owned(),
                kernel_release,
                criu_version: criu.version,
                criu_check_supported: criu.basic_supported,
                criu_check_diagnostic: criu.basic_diagnostic.clone(),
                criu_check_all_supported: criu.extended_supported,
                criu_check_all_diagnostic: criu.extended_diagnostic,
                effective_uid: effective_uid(),
            },
            diagnostic: criu.basic_diagnostic,
        })
    }

    fn probe_available_criu(
        &self,
        temporary: &TempDir,
        artifacts: &mut Vec<LogArtifact>,
    ) -> Result<CriuProbe, RunError> {
        let version = run_bounded(
            &self.config.criu_path,
            &[OsString::from("--version")],
            temporary.path(),
            self.config.limits.dump_timeout_ms,
            self.config.limits.max_log_bytes,
            "logs/environment-criu-version.log",
        )?;
        let version_text = successful_first_line(&version);
        artifacts.push(version.artifact());
        let check = run_bounded(
            &self.config.criu_path,
            &[OsString::from("check")],
            temporary.path(),
            self.config.limits.dump_timeout_ms,
            self.config.limits.max_log_bytes,
            "logs/environment-criu-check.log",
        )?;
        let check_supported = check.success();
        let diagnostic = if check_supported {
            Diagnostic {
                code: DiagnosticCode::Supported,
                stage: "criu_check".to_owned(),
                message: "criu check succeeded".to_owned(),
                log_artifact: check.measurement.log_artifact.clone(),
            }
        } else {
            Diagnostic {
                code: DiagnosticCode::CriuCheckFailed,
                stage: "criu_check".to_owned(),
                message: summarize_command_failure("criu check", &check),
                log_artifact: check.measurement.log_artifact.clone(),
            }
        };
        artifacts.push(check.artifact());
        let check_all = run_bounded(
            &self.config.criu_path,
            &[OsString::from("check"), OsString::from("--all")],
            temporary.path(),
            self.config.limits.dump_timeout_ms,
            self.config.limits.max_log_bytes,
            "logs/environment-criu-check-all.log",
        )?;
        let check_all_supported = check_all.success();
        let check_all_diagnostic = Diagnostic {
            code: if check_all_supported {
                DiagnosticCode::Supported
            } else {
                DiagnosticCode::CriuCheckFailed
            },
            stage: "criu_check_all".to_owned(),
            message: if check_all_supported {
                "criu check --all succeeded".to_owned()
            } else {
                summarize_command_failure("criu check --all", &check_all)
            },
            log_artifact: check_all.measurement.log_artifact.clone(),
        };
        artifacts.push(check_all.artifact());
        Ok(CriuProbe {
            version: version_text,
            basic_supported: check_supported,
            basic_diagnostic: diagnostic,
            extended_supported: check_all_supported,
            extended_diagnostic: check_all_diagnostic,
        })
    }

    fn run_supported_matrix(
        &self,
        temporary: &TempDir,
        artifacts: &mut Vec<LogArtifact>,
    ) -> Result<Vec<CompatibilityRow>, RunError> {
        let mut rows = Vec::new();
        for (index, (scenario, scale)) in matrix_points(&self.config.scaling).enumerate() {
            if scenario == Scenario::ExternalTcp {
                rows.push(external_tcp_row(index, scale, artifacts));
            } else {
                rows.push(self.run_scenario(temporary, index, scenario, scale, artifacts)?);
            }
        }
        Ok(rows)
    }

    fn run_scenario(
        &self,
        temporary: &TempDir,
        index: usize,
        scenario: Scenario,
        scale: ScalePoint,
        artifacts: &mut Vec<LogArtifact>,
    ) -> Result<CompatibilityRow, RunError> {
        let root = TempBuilder::new()
            .prefix(&format!("row-{index:04}-"))
            .tempdir_in(temporary.path())?;
        let workspace = root.path().join("workspace");
        let images = root.path().join("images");
        fs::create_dir(&workspace)?;
        fs::create_dir(&images)?;
        let fixture_log_path = root.path().join("fixture-stderr.log");
        let fixture_log = File::create(&fixture_log_path)?;
        let mut fixture = spawn_fixture(
            &self.config.fixture_path,
            scenario,
            scale,
            &workspace,
            fixture_log,
        )?;
        let fixture_pid = fixture.id();
        let ready = wait_for_fixture(&mut fixture, &workspace, Duration::from_secs(5))?;
        if !ready {
            terminate_child_group(&mut fixture);
            let log_artifact = format!("logs/row-{index:04}-diagnostic.log");
            let log = read_bounded(&fixture_log_path, self.config.limits.max_log_bytes)?;
            artifacts.push(LogArtifact {
                relative_path: log_artifact.clone(),
                bytes: log.bytes,
            });
            return Ok(failed_without_commands(
                scenario,
                scale,
                DiagnosticCode::FixtureFailed,
                "fixture did not become ready before exit or timeout",
                log_artifact,
            ));
        }
        let before = behavior_snapshot(scenario, scale, &workspace)?;
        let dump_artifact = format!("logs/row-{index:04}-dump.log");
        let dump_arguments = criu_dump_arguments(fixture_pid, &images);
        let mut dump = run_bounded(
            &self.config.criu_path,
            &dump_arguments,
            root.path(),
            self.config.limits.dump_timeout_ms,
            self.config.limits.max_log_bytes,
            &dump_artifact,
        )?;
        merge_criu_log(
            &mut dump,
            &images.join("dump.log"),
            self.config.limits.max_log_bytes,
        )?;
        artifacts.push(dump.artifact());
        if !dump.success() {
            terminate_child_group(&mut fixture);
            let (status, code) = command_failure_classification(&dump, true);
            return Ok(row_from_command_failure(
                scenario,
                scale,
                status,
                code,
                "dump",
                &dump,
                Some(dump.measurement.clone()),
                None,
                index,
                artifacts,
            ));
        }
        let _ = fixture.wait();
        self.restore_and_verify(
            RestoreContext {
                root: root.path(),
                workspace: &workspace,
                images: &images,
                before: &before,
                index,
                scenario,
                scale,
            },
            dump,
            artifacts,
        )
    }

    fn restore_and_verify(
        &self,
        context: RestoreContext<'_>,
        dump: CommandRun,
        artifacts: &mut Vec<LogArtifact>,
    ) -> Result<CompatibilityRow, RunError> {
        let RestoreContext {
            root,
            workspace,
            images,
            before,
            index,
            scenario,
            scale,
        } = context;
        let image_bytes = directory_bytes(images)?;
        let restore_artifact = format!("logs/row-{index:04}-restore.log");
        let restored_pid_path = root.join("restored.pid");
        let restore_arguments = criu_restore_arguments(images, &restored_pid_path);
        let mut restore = run_bounded(
            &self.config.criu_path,
            &restore_arguments,
            root,
            self.config.limits.restore_timeout_ms,
            self.config.limits.max_log_bytes,
            &restore_artifact,
        )?;
        merge_criu_log(
            &mut restore,
            &images.join("restore.log"),
            self.config.limits.max_log_bytes,
        )?;
        artifacts.push(restore.artifact());
        if !restore.success() {
            let (status, code) = command_failure_classification(&restore, false);
            return Ok(row_from_command_failure(
                scenario,
                scale,
                status,
                code,
                "restore",
                &restore,
                Some(dump.measurement),
                Some(restore.measurement.clone()),
                index,
                artifacts,
            ));
        }
        let restored_pid = read_pid(&restored_pid_path)?;
        let verified = wait_for_behavior(
            scenario,
            scale,
            workspace,
            before,
            Duration::from_millis(self.config.limits.restore_timeout_ms.min(5_000)),
        )?;
        let cleanup_ok = terminate_restored_group(restored_pid);
        let diagnostic_artifact = format!("logs/row-{index:04}-diagnostic.log");
        let (status, code, message) = if !verified {
            (
                RowStatus::Failed,
                DiagnosticCode::VerificationFailed,
                "restore exited successfully but observable behavior did not resume".to_owned(),
            )
        } else if !cleanup_ok {
            (
                RowStatus::Failed,
                DiagnosticCode::CleanupFailed,
                "restored behavior was verified but process-group cleanup failed".to_owned(),
            )
        } else {
            (
                RowStatus::Supported,
                DiagnosticCode::Supported,
                "dump, restore, behavior verification, and cleanup succeeded".to_owned(),
            )
        };
        artifacts.push(LogArtifact {
            relative_path: diagnostic_artifact.clone(),
            bytes: format!("verification={verified}\ncleanup={cleanup_ok}\n{message}\n")
                .into_bytes(),
        });
        Ok(CompatibilityRow {
            scenario,
            scale,
            status,
            diagnostic: Diagnostic {
                code,
                stage: if status == RowStatus::Supported {
                    "complete".to_owned()
                } else {
                    "verification".to_owned()
                },
                message,
                log_artifact: diagnostic_artifact,
            },
            dump: Some(dump.measurement),
            restore: Some(restore.measurement),
            image_bytes: Some(image_bytes),
            restored_behavior_verified: verified,
        })
    }
}

#[derive(Clone, Copy)]
struct RestoreContext<'a> {
    root: &'a Path,
    workspace: &'a Path,
    images: &'a Path,
    before: &'a BehaviorSnapshot,
    index: usize,
    scenario: Scenario,
    scale: ScalePoint,
}

#[derive(Clone, Debug)]
struct EnvironmentProbe {
    environment: EnvironmentMetadata,
    diagnostic: Diagnostic,
}

#[derive(Clone, Debug)]
struct CriuProbe {
    version: Option<String>,
    basic_supported: bool,
    basic_diagnostic: Diagnostic,
    extended_supported: bool,
    extended_diagnostic: Diagnostic,
}

fn unavailable_diagnostic(config: &RunnerConfig) -> Diagnostic {
    let (code, message) = if std::env::consts::OS != "linux" {
        (
            DiagnosticCode::CriuUnavailable,
            format!(
                "CRIU process checkpoint experiments require Linux; current OS is {}",
                std::env::consts::OS
            ),
        )
    } else if !is_executable_file(&config.criu_path) {
        (
            DiagnosticCode::CriuUnavailable,
            format!(
                "CRIU executable is unavailable at {}",
                config.criu_path.display()
            ),
        )
    } else if !is_executable_file(&config.fixture_path) {
        (
            DiagnosticCode::FixtureFailed,
            format!(
                "compatibility fixture is unavailable at {}",
                config.fixture_path.display()
            ),
        )
    } else {
        (
            DiagnosticCode::CriuCheckFailed,
            "CRIU requires a successful bounded feature check before scenarios run".to_owned(),
        )
    };
    Diagnostic {
        code,
        stage: "discovery".to_owned(),
        message,
        log_artifact: "logs/environment-criu-check.log".to_owned(),
    }
}

fn unsupported_rows(
    scaling: &ScalingPlan,
    environment_diagnostic: &Diagnostic,
    artifacts: &mut Vec<LogArtifact>,
) -> Vec<CompatibilityRow> {
    matrix_points(scaling)
        .enumerate()
        .map(|(index, (scenario, scale))| {
            if scenario == Scenario::ExternalTcp {
                external_tcp_row(index, scale, artifacts)
            } else {
                let log_artifact = format!("logs/row-{index:04}-diagnostic.log");
                let diagnostic = Diagnostic {
                    log_artifact: log_artifact.clone(),
                    ..environment_diagnostic.clone()
                };
                artifacts.push(LogArtifact {
                    relative_path: log_artifact,
                    bytes: format!("{}: {}\n", diagnostic.stage, diagnostic.message).into_bytes(),
                });
                CompatibilityRow {
                    scenario,
                    scale,
                    status: RowStatus::Unsupported,
                    diagnostic,
                    dump: None,
                    restore: None,
                    image_bytes: None,
                    restored_behavior_verified: false,
                }
            }
        })
        .collect()
}

fn external_tcp_row(
    index: usize,
    scale: ScalePoint,
    artifacts: &mut Vec<LogArtifact>,
) -> CompatibilityRow {
    let log_artifact = format!("logs/row-{index:04}-diagnostic.log");
    let message = "external TCP depends on remote peer state and is explicitly outside the transparent restore subset";
    artifacts.push(LogArtifact {
        relative_path: log_artifact.clone(),
        bytes: format!("scenario: {message}\n").into_bytes(),
    });
    CompatibilityRow {
        scenario: Scenario::ExternalTcp,
        scale,
        status: RowStatus::Unsupported,
        diagnostic: Diagnostic {
            code: DiagnosticCode::ExternalTcpUnsupported,
            stage: "scenario".to_owned(),
            message: message.to_owned(),
            log_artifact,
        },
        dump: None,
        restore: None,
        image_bytes: None,
        restored_behavior_verified: false,
    }
}

fn failed_without_commands(
    scenario: Scenario,
    scale: ScalePoint,
    code: DiagnosticCode,
    message: &str,
    log_artifact: String,
) -> CompatibilityRow {
    CompatibilityRow {
        scenario,
        scale,
        status: RowStatus::Failed,
        diagnostic: Diagnostic {
            code,
            stage: "fixture".to_owned(),
            message: message.to_owned(),
            log_artifact,
        },
        dump: None,
        restore: None,
        image_bytes: None,
        restored_behavior_verified: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn row_from_command_failure(
    scenario: Scenario,
    scale: ScalePoint,
    status: RowStatus,
    code: DiagnosticCode,
    stage: &str,
    command: &CommandRun,
    dump: Option<CommandMeasurement>,
    restore: Option<CommandMeasurement>,
    index: usize,
    artifacts: &mut Vec<LogArtifact>,
) -> CompatibilityRow {
    let log_artifact = format!("logs/row-{index:04}-diagnostic.log");
    let message = summarize_command_failure(stage, command);
    artifacts.push(LogArtifact {
        relative_path: log_artifact.clone(),
        bytes: format!("{stage}: {message}\n").into_bytes(),
    });
    CompatibilityRow {
        scenario,
        scale,
        status,
        diagnostic: Diagnostic {
            code,
            stage: stage.to_owned(),
            message,
            log_artifact,
        },
        dump,
        restore,
        image_bytes: None,
        restored_behavior_verified: false,
    }
}

fn command_failure_classification(command: &CommandRun, dump: bool) -> (RowStatus, DiagnosticCode) {
    if command.measurement.timed_out {
        return (
            RowStatus::Failed,
            if dump {
                DiagnosticCode::DumpTimedOut
            } else {
                DiagnosticCode::RestoreTimedOut
            },
        );
    }
    let log = String::from_utf8_lossy(&command.log).to_ascii_lowercase();
    let unsupported = [
        "not supported",
        "unsupported",
        "operation not permitted",
        "permission denied",
        "apparmor",
        "could not check",
    ]
    .iter()
    .any(|pattern| log.contains(pattern));
    if unsupported {
        (
            RowStatus::Unsupported,
            if dump {
                DiagnosticCode::DumpUnsupported
            } else {
                DiagnosticCode::RestoreUnsupported
            },
        )
    } else {
        (
            RowStatus::Failed,
            if dump {
                DiagnosticCode::DumpFailed
            } else {
                DiagnosticCode::RestoreFailed
            },
        )
    }
}

fn decide(rows: &[CompatibilityRow], thresholds: DecisionThresholds) -> DecisionEvidence {
    let considered = rows
        .iter()
        .filter(|row| row.scenario != Scenario::ExternalTcp)
        .collect::<Vec<_>>();
    let supported = considered
        .iter()
        .copied()
        .filter(|row| row.status == RowStatus::Supported)
        .collect::<Vec<_>>();
    if supported.is_empty() {
        return DecisionEvidence {
            recommendation: Recommendation::Kill,
            rationale: "No declared transparent-restore scenario produced verified support."
                .to_owned(),
        };
    }
    if considered.iter().any(|row| row.status == RowStatus::Failed) {
        return DecisionEvidence {
            recommendation: Recommendation::Kill,
            rationale:
                "At least one attempted declared row failed, violating the 100% correctness gate."
                    .to_owned(),
        };
    }
    let dump_p95 = percentile_95(
        supported
            .iter()
            .filter_map(|row| row.dump.as_ref().map(|value| value.latency_ms))
            .collect(),
    );
    let restore_p95 = percentile_95(
        supported
            .iter()
            .filter_map(|row| row.restore.as_ref().map(|value| value.latency_ms))
            .collect(),
    );
    if supported.len() == considered.len()
        && dump_p95.is_some_and(|value| value <= thresholds.checkpoint_pause_p95_ms)
        && restore_p95.is_some_and(|value| value <= thresholds.restore_p95_ms)
    {
        DecisionEvidence {
            recommendation: Recommendation::Keep,
            rationale: "Every declared in-scope row restored correctly within the preliminary latency gates."
                .to_owned(),
        }
    } else {
        DecisionEvidence {
            recommendation: Recommendation::Narrow,
            rationale: format!(
                "Verified support is limited to {}/{} in-scope rows (dump p95 {} ms, restore p95 {} ms).",
                supported.len(),
                considered.len(),
                dump_p95.map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
                restore_p95.map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
            ),
        }
    }
}

fn percentile_95(mut values: Vec<u64>) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let rank = values.len().saturating_mul(95).div_ceil(100).max(1);
    values.get(rank - 1).copied()
}

#[derive(Debug)]
struct CommandRun {
    measurement: CommandMeasurement,
    status: Option<ExitStatus>,
    log: Vec<u8>,
    launch_error: Option<String>,
}

impl CommandRun {
    fn success(&self) -> bool {
        self.status.is_some_and(|status| status.success())
            && !self.measurement.timed_out
            && self.launch_error.is_none()
    }

    fn artifact(&self) -> LogArtifact {
        LogArtifact {
            relative_path: self.measurement.log_artifact.clone(),
            bytes: self.log.clone(),
        }
    }
}

fn run_bounded(
    program: &Path,
    arguments: &[OsString],
    working_directory: &Path,
    timeout_ms: u64,
    max_log_bytes: usize,
    log_artifact: &str,
) -> Result<CommandRun, RunError> {
    let io_root = TempBuilder::new()
        .prefix("command-")
        .tempdir_in(working_directory)?;
    let stdout_path = io_root.path().join("stdout");
    let stderr_path = io_root.path().join("stderr");
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;
    let started = Instant::now();
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(working_directory)
        .env_clear()
        .env("HOME", working_directory)
        .env("LC_ALL", "C")
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let message = format!("failed to launch {}: {error}\n", program.display());
            return Ok(CommandRun {
                measurement: CommandMeasurement {
                    exit_code: None,
                    timed_out: false,
                    latency_ms: elapsed_ms(started),
                    log_artifact: log_artifact.to_owned(),
                    log_truncated: message.len() > max_log_bytes,
                },
                status: None,
                log: message
                    .into_bytes()
                    .into_iter()
                    .take(max_log_bytes)
                    .collect(),
                launch_error: Some(error.to_string()),
            });
        }
    };
    let deadline = Duration::from_millis(timeout_ms);
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (Some(status), false);
        }
        if started.elapsed() >= deadline {
            terminate_command_group(&mut child);
            break (child.wait().ok(), true);
        }
        thread::sleep(Duration::from_millis(10));
    };
    let mut stdout = read_bounded(&stdout_path, max_log_bytes)?;
    let remaining = max_log_bytes.saturating_sub(stdout.bytes.len());
    let stderr = read_bounded(&stderr_path, remaining)?;
    stdout.bytes.extend(stderr.bytes);
    Ok(CommandRun {
        measurement: CommandMeasurement {
            exit_code: status.and_then(|value| value.code()),
            timed_out,
            latency_ms: elapsed_ms(started),
            log_artifact: log_artifact.to_owned(),
            log_truncated: stdout.truncated || stderr.truncated,
        },
        status,
        log: stdout.bytes,
        launch_error: None,
    })
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

struct BoundedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_bounded(path: &Path, maximum: usize) -> Result<BoundedRead, RunError> {
    let file = File::open(path)?;
    let requested = maximum.saturating_add(1);
    let mut bytes = Vec::with_capacity(requested.min(64 * 1024));
    file.take(u64::try_from(requested).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > maximum;
    bytes.truncate(maximum);
    Ok(BoundedRead { bytes, truncated })
}

fn merge_criu_log(command: &mut CommandRun, path: &Path, maximum: usize) -> Result<(), RunError> {
    if !path.is_file() {
        return Ok(());
    }
    let remaining = maximum.saturating_sub(command.log.len());
    let internal = read_bounded(path, remaining)?;
    command.log.extend(internal.bytes);
    command.measurement.log_truncated |= internal.truncated;
    Ok(())
}

fn successful_first_line(command: &CommandRun) -> Option<String> {
    command.success().then(|| {
        String::from_utf8_lossy(&command.log)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()
    })
}

fn summarize_command_failure(name: &str, command: &CommandRun) -> String {
    if command.measurement.timed_out {
        return format!(
            "{name} timed out after {} ms",
            command.measurement.latency_ms
        );
    }
    if let Some(error) = &command.launch_error {
        return format!("{name} could not launch: {error}");
    }
    let decoded = String::from_utf8_lossy(&command.log);
    let excerpt = decoded
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no diagnostic output")
        .trim();
    format!(
        "{name} exited {:?}: {excerpt}",
        command.measurement.exit_code
    )
}

fn spawn_fixture(
    executable: &Path,
    scenario: Scenario,
    scale: ScalePoint,
    workspace: &Path,
    stderr: File,
) -> Result<Child, RunError> {
    let mut command = Command::new(executable);
    command
        .args([
            OsString::from("--scenario"),
            OsString::from(scenario.as_str()),
            OsString::from("--workspace"),
            workspace.as_os_str().to_owned(),
            OsString::from("--memory-bytes"),
            OsString::from(scale.memory_bytes.to_string()),
            OsString::from("--process-count"),
            OsString::from(scale.process_count.to_string()),
        ])
        .current_dir(workspace)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    command.spawn().map_err(RunError::from)
}

fn wait_for_fixture(
    child: &mut Child,
    workspace: &Path,
    timeout: Duration,
) -> Result<bool, RunError> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if workspace.join("ready").is_file() && read_counter(&workspace.join("heartbeat")).is_some()
        {
            return Ok(true);
        }
        if child.try_wait()?.is_some() {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(false)
}

#[derive(Clone, Debug)]
struct BehaviorSnapshot {
    heartbeat: u64,
    children: Vec<u64>,
    open_file_bytes: Option<u64>,
    mutation_revision: Option<u64>,
}

fn behavior_snapshot(
    scenario: Scenario,
    scale: ScalePoint,
    workspace: &Path,
) -> Result<BehaviorSnapshot, RunError> {
    let heartbeat = read_counter(&workspace.join("heartbeat")).unwrap_or(0);
    let children = if scenario == Scenario::ProcessTree {
        (1..scale.process_count)
            .map(|index| {
                read_counter(&workspace.join(format!("child-{index}-heartbeat"))).unwrap_or(0)
            })
            .collect()
    } else {
        Vec::new()
    };
    let open_file_bytes = (scenario == Scenario::OpenRegularFile)
        .then(|| fs::metadata(workspace.join("open-file.log")).map(|value| value.len()))
        .transpose()?;
    let mutation_revision = (scenario == Scenario::WorkspaceMutation)
        .then(|| read_revision(&workspace.join("workspace-mutation.txt")))
        .flatten();
    Ok(BehaviorSnapshot {
        heartbeat,
        children,
        open_file_bytes,
        mutation_revision,
    })
}

fn wait_for_behavior(
    scenario: Scenario,
    scale: ScalePoint,
    workspace: &Path,
    before: &BehaviorSnapshot,
    timeout: Duration,
) -> Result<bool, RunError> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        let now = behavior_snapshot(scenario, scale, workspace)?;
        let heartbeat = now.heartbeat > before.heartbeat;
        let children = now.children.len() == before.children.len()
            && now
                .children
                .iter()
                .zip(&before.children)
                .all(|(after, prior)| after > prior);
        let open_file = match (now.open_file_bytes, before.open_file_bytes) {
            (Some(after), Some(prior)) => after > prior,
            (None, None) => true,
            _ => false,
        };
        let mutation = match (now.mutation_revision, before.mutation_revision) {
            (Some(after), Some(prior)) => after > prior,
            (None, None) => true,
            _ => false,
        };
        if heartbeat && children && open_file && mutation {
            return Ok(true);
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(false)
}

fn read_counter(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_revision(path: &Path) -> Option<u64> {
    fs::read_to_string(path)
        .ok()?
        .trim()
        .strip_prefix("revision=")?
        .parse()
        .ok()
}

fn criu_dump_arguments(pid: u32, images: &Path) -> Vec<OsString> {
    vec![
        "dump".into(),
        "--tree".into(),
        pid.to_string().into(),
        "--images-dir".into(),
        images.as_os_str().to_owned(),
        "--work-dir".into(),
        images.as_os_str().to_owned(),
        "--log-file".into(),
        "dump.log".into(),
        "--shell-job".into(),
        "--tcp-established".into(),
        "--file-locks".into(),
        "--manage-cgroups=ignore".into(),
    ]
}

fn criu_restore_arguments(images: &Path, pid_file: &Path) -> Vec<OsString> {
    vec![
        "restore".into(),
        "--images-dir".into(),
        images.as_os_str().to_owned(),
        "--work-dir".into(),
        images.as_os_str().to_owned(),
        "--log-file".into(),
        "restore.log".into(),
        "--shell-job".into(),
        "--tcp-established".into(),
        "--file-locks".into(),
        "--manage-cgroups=ignore".into(),
        "--restore-detached".into(),
        "--pidfile".into(),
        pid_file.as_os_str().to_owned(),
    ]
}

fn directory_bytes(path: &Path) -> Result<u64, RunError> {
    let mut total = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    Ok(total)
}

fn read_pid(path: &Path) -> Result<u32, RunError> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|_| RunError::InvalidRestoredPid)
}

fn terminate_child_group(child: &mut Child) {
    #[cfg(unix)]
    {
        if let Ok(pid) = i32::try_from(child.id()) {
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn terminate_command_group(child: &mut Child) {
    terminate_child_group(child);
}

fn terminate_restored_group(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        match killpg(Pid::from_raw(pid), Signal::SIGKILL) {
            Ok(()) | Err(Errno::ESRCH) => true,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

#[cfg(target_os = "linux")]
fn effective_uid() -> Option<u32> {
    fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|line| line.starts_with("Uid:"))?
        .split_whitespace()
        .nth(2)?
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
const fn effective_uid() -> Option<u32> {
    None
}

#[cfg(unix)]
fn set_private_run_directory(path: &Path) -> Result<(), RunError> {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
const fn set_private_run_directory(_path: &Path) -> Result<(), RunError> {
    Ok(())
}

fn matrix_points(scaling: &ScalingPlan) -> impl Iterator<Item = (Scenario, ScalePoint)> {
    let mut points = Vec::new();
    for scenario in Scenario::declared() {
        match scenario {
            Scenario::ProcessTree => {
                for memory_bytes in &scaling.memory_bytes {
                    for process_count in &scaling.process_counts {
                        points.push((
                            *scenario,
                            ScalePoint {
                                memory_bytes: *memory_bytes,
                                process_count: *process_count,
                            },
                        ));
                    }
                }
            }
            Scenario::ExternalTcp => points.push((
                *scenario,
                ScalePoint {
                    memory_bytes: scaling.memory_bytes[0],
                    process_count: 1,
                },
            )),
            _ => points.extend(scaling.memory_bytes.iter().map(|memory_bytes| {
                (
                    *scenario,
                    ScalePoint {
                        memory_bytes: *memory_bytes,
                        process_count: 1,
                    },
                )
            })),
        }
    }
    points.into_iter()
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
    #[error("CRIU restore produced an invalid PID file")]
    InvalidRestoredPid,
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
