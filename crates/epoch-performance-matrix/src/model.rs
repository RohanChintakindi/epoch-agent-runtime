use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 1;
pub const MIB: u64 = 1024 * 1024;
pub const GIB: u64 = 1024 * MIB;
pub const REQUIRED_ALLOCATIONS_BYTES: &[u64] = &[128 * MIB, 512 * MIB, GIB];
pub const REQUIRED_FANOUTS: &[u16] = &[1, 2, 4, 8];
pub const REQUIRED_DIRTY_BASIS_POINTS: &[u16] = &[0, 100, 1_000, 5_000, 10_000];

#[derive(Debug, Error)]
pub enum MatrixError {
    #[error("code revision must be exactly 40 lowercase hexadecimal characters")]
    InvalidRevision,
    #[error("evidence output already exists: {0}")]
    OutputExists(PathBuf),
    #[error("evidence output has no existing parent: {0}")]
    MissingOutputParent(PathBuf),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Validates an exact lowercase Git object ID.
///
/// # Errors
///
/// Rejects symbolic, abbreviated, uppercase, or dirty-suffixed revisions.
pub fn validate_revision(revision: &str) -> Result<&str, MatrixError> {
    if revision.len() == 40
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(revision)
    } else {
        Err(MatrixError::InvalidRevision)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostMemory {
    pub available_bytes: u64,
    pub safety_budget_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkEnvironment {
    pub operating_system: String,
    pub architecture: String,
    pub kernel_release: String,
    pub logical_cpus: usize,
    pub code_revision: String,
    pub host_memory: HostMemory,
}

impl BenchmarkEnvironment {
    #[must_use]
    pub fn synthetic_non_linux(
        operating_system: &str,
        architecture: &str,
        host_memory: HostMemory,
    ) -> Self {
        Self {
            operating_system: operating_system.to_owned(),
            architecture: architecture.to_owned(),
            kernel_release: "synthetic".to_owned(),
            logical_cpus: 1,
            code_revision: String::new(),
            host_memory,
        }
    }

    #[must_use]
    pub fn is_linux(&self) -> bool {
        self.operating_system == "linux"
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CowMatrixConfig {
    pub allocations_bytes: Vec<u64>,
    pub fanouts: Vec<u16>,
    pub dirty_basis_points: Vec<u16>,
    pub repetitions: u16,
    pub helper: Option<PathBuf>,
    pub python: PathBuf,
    pub timeout_ms: u64,
}

impl CowMatrixConfig {
    #[must_use]
    pub fn required() -> Self {
        Self {
            allocations_bytes: REQUIRED_ALLOCATIONS_BYTES.to_vec(),
            fanouts: REQUIRED_FANOUTS.to_vec(),
            dirty_basis_points: REQUIRED_DIRTY_BASIS_POINTS.to_vec(),
            repetitions: 3,
            helper: None,
            python: PathBuf::from("/usr/bin/python3"),
            timeout_ms: 180_000,
        }
    }

    #[must_use]
    pub fn include_optional_2gib(mut self) -> Self {
        if !self.allocations_bytes.contains(&(2 * GIB)) {
            self.allocations_bytes.push(2 * GIB);
            self.allocations_bytes.sort_unstable();
        }
        self
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CowRowKey {
    pub allocation_bytes: u64,
    pub fanout: u16,
    pub dirty_basis_points: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PlannedOutcome {
    Planned {
        estimated_peak_bytes: u64,
    },
    Skipped {
        code: String,
        detail: String,
        estimated_peak_bytes: u64,
    },
    Unsupported {
        code: String,
        detail: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlannedCowRow {
    pub key: CowRowKey,
    pub outcome: PlannedOutcome,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CowSample {
    pub ordinal: u16,
    pub runtime_ns: u64,
    pub allocation_ns: u64,
    pub fork_pause_ns: u64,
    pub dirty_ns_max: u64,
    pub minor_faults: u64,
    pub major_faults: u64,
    pub cow_rss_bytes: u64,
    pub cow_pss_bytes: u64,
    pub full_copy_bytes: u64,
    pub full_copy_ns: u64,
    pub pss_to_full_copy_basis_points: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Percentiles {
    pub minimum: u64,
    pub p50: u64,
    pub p95: u64,
    pub maximum: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CowPointSummary {
    pub runtime_ns: Percentiles,
    pub fork_pause_ns: Percentiles,
    pub cow_rss_bytes: Percentiles,
    pub cow_pss_bytes: Percentiles,
    pub minor_faults: Percentiles,
    pub major_faults: Percentiles,
    pub pss_to_full_copy_basis_points: Percentiles,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub detail: String,
}

impl Diagnostic {
    #[must_use]
    pub fn new(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CowResultRow {
    pub key: CowRowKey,
    pub status: String,
    pub estimated_peak_bytes: Option<u64>,
    pub diagnostic: Option<Diagnostic>,
    pub samples: Vec<CowSample>,
    pub summary: Option<CowPointSummary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CowMatrixSummary {
    pub total_rows: usize,
    pub supported_rows: usize,
    pub skipped_rows: usize,
    pub unsupported_rows: usize,
    pub failed_rows: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CowMatrixReport {
    pub rows: Vec<CowResultRow>,
    pub summary: CowMatrixSummary,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendLabel {
    Direct,
    Linux,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplePhase {
    Cold,
    Warm,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleStatus {
    Supported,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IsolationSample {
    pub backend: BackendLabel,
    pub phase: SamplePhase,
    pub ordinal: u16,
    pub status: SampleStatus,
    pub total_ns: u64,
    pub launch_overhead_ns: u64,
    pub workload_runtime_ns: u64,
    pub cpu_user_ns: u64,
    pub cpu_system_ns: u64,
    pub peak_rss_bytes: u64,
    pub compatibility: String,
    pub diagnostic: Option<Diagnostic>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IsolationSummary {
    pub cold_total_ns: u64,
    pub warm_total_p50_ns: u64,
    pub warm_total_p95_ns: u64,
    pub warm_launch_overhead_p50_ns: u64,
    pub warm_cpu_p50_ns: u64,
    pub peak_rss_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointInteraction {
    pub backend: Option<BackendLabel>,
    pub status: String,
    pub detail: String,
}

impl CheckpointInteraction {
    #[must_use]
    pub fn unsupported(detail: impl Into<String>) -> Self {
        Self {
            backend: None,
            status: "unsupported".to_owned(),
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn for_backend(mut self, backend: BackendLabel) -> Self {
        self.backend = Some(backend);
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackendIsolationReport {
    pub backend: BackendLabel,
    pub status: String,
    pub diagnostic: Option<Diagnostic>,
    pub samples: Vec<IsolationSample>,
    pub summary: Option<IsolationSummary>,
    pub checkpoint_interaction: CheckpointInteraction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IsolationComparison {
    pub status: String,
    pub direct: BackendIsolationReport,
    pub linux: BackendIsolationReport,
    pub checkpoint_interactions: Vec<CheckpointInteraction>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IsolationConfig {
    pub repetitions: u16,
    pub probe: Option<PathBuf>,
    pub trusted_sandbox_helper: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    pub memory_limit_bytes: u64,
    pub pids_limit: u32,
    pub cpu_percent: u16,
}

impl IsolationConfig {
    #[must_use]
    pub fn disabled_fixture() -> Self {
        Self {
            repetitions: 3,
            probe: None,
            trusted_sandbox_helper: None,
            workspace: None,
            memory_limit_bytes: 128 * MIB,
            pids_limit: 16,
            cpu_percent: 100,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerformanceConfig {
    pub code_revision: String,
    pub cow: CowMatrixConfig,
    pub isolation: IsolationConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PerformanceReport {
    pub schema_version: u32,
    pub config: PerformanceConfig,
    pub environment: BenchmarkEnvironment,
    pub cow: CowMatrixReport,
    pub isolation: IsolationComparison,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactBundle {
    pub json: PathBuf,
    pub csv: PathBuf,
    pub markdown: PathBuf,
    pub checksums: PathBuf,
}

impl ArtifactBundle {
    #[must_use]
    pub fn at(root: &Path) -> Self {
        Self {
            json: root.join("report.json"),
            csv: root.join("samples.csv"),
            markdown: root.join("RESULTS.md"),
            checksums: root.join("checksums.sha256"),
        }
    }
}

pub(crate) fn percentile(values: impl IntoIterator<Item = u64>, percentile: usize) -> u64 {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let numerator = percentile.saturating_mul(values.len().saturating_sub(1));
    values[numerator.div_ceil(100).min(values.len() - 1)]
}

pub(crate) fn percentiles(values: impl IntoIterator<Item = u64>) -> Percentiles {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_unstable();
    Percentiles {
        minimum: values.first().copied().unwrap_or(0),
        p50: percentile(values.iter().copied(), 50),
        p95: percentile(values.iter().copied(), 95),
        maximum: values.last().copied().unwrap_or(0),
    }
}
