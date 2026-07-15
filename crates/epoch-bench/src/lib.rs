//! Reproducible benchmark contracts for Epoch runtime experiments.

use std::{collections::BTreeMap, fmt::Write as _};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema version emitted by this crate.
pub const REPORT_SCHEMA_VERSION: u32 = 1;
/// Upper bound protecting accidental multi-hour warmup configurations.
pub const MAX_WARMUPS: u32 = 10_000;
/// Upper bound protecting accidental unbounded benchmark configurations.
pub const MAX_REPETITIONS: u32 = 100_000;

/// Whether boundary tracing is enabled for a benchmark run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceMode {
    /// Measure without Epoch trace capture.
    Off,
    /// Measure with Epoch trace capture.
    On,
}

impl TraceMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }
}

/// Validated configuration that makes one benchmark run reproducible.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkConfig {
    /// Stable suite name.
    pub suite: String,
    /// Backend under test.
    pub backend: String,
    /// Trace mode, kept authoritative so traced and untraced samples cannot be mixed.
    pub trace_mode: TraceMode,
    /// Root seed used to derive one distinct seed per iteration.
    pub seed: u64,
    /// Number of discarded warmup iterations.
    pub warmups: u32,
    /// Number of retained measurement iterations.
    pub repetitions: u32,
}

impl BenchmarkConfig {
    /// Validates and constructs a benchmark configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when names are empty or iteration counts exceed safety bounds.
    pub fn new(
        suite: &str,
        backend: &str,
        trace_mode: TraceMode,
        seed: u64,
        warmups: u32,
        repetitions: u32,
    ) -> Result<Self, ConfigError> {
        let suite = suite.trim();
        if suite.is_empty() {
            return Err(ConfigError::EmptySuite);
        }
        let backend = backend.trim();
        if backend.is_empty() {
            return Err(ConfigError::EmptyBackend);
        }
        if warmups > MAX_WARMUPS {
            return Err(ConfigError::InvalidWarmups { value: warmups });
        }
        if repetitions == 0 || repetitions > MAX_REPETITIONS {
            return Err(ConfigError::InvalidRepetitions { value: repetitions });
        }

        Ok(Self {
            suite: suite.to_owned(),
            backend: backend.to_owned(),
            trace_mode,
            seed,
            warmups,
            repetitions,
        })
    }
}

/// Configuration validation failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ConfigError {
    /// Suite name was empty or whitespace.
    #[error("benchmark suite must not be empty")]
    EmptySuite,
    /// Backend name was empty or whitespace.
    #[error("benchmark backend must not be empty")]
    EmptyBackend,
    /// Warmup count exceeded the configured safety bound.
    #[error("warmup count {value} exceeds maximum {MAX_WARMUPS}")]
    InvalidWarmups {
        /// Rejected value.
        value: u32,
    },
    /// Repetition count was zero or exceeded the safety bound.
    #[error("repetition count {value} must be between 1 and {MAX_REPETITIONS}")]
    InvalidRepetitions {
        /// Rejected value.
        value: u32,
    },
}

/// Environment facts attached to every benchmark report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkEnvironment {
    /// Exact source revision or dirty-tree identifier.
    pub code_revision: String,
    /// Operating system name.
    pub os: String,
    /// CPU architecture.
    pub architecture: String,
    /// Kernel release.
    pub kernel_release: String,
    /// Logical CPUs visible to the benchmark process.
    pub cpu_count: u32,
    /// Host memory when discoverable.
    pub total_memory_bytes: Option<u64>,
    /// Runtime/compiler version used for the experiment.
    pub runtime_version: String,
    /// Stable additional metadata such as VM shape.
    pub extra: BTreeMap<String, String>,
}

/// One requested scenario invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Iteration {
    /// Zero-based ordinal within warmup or retained measurements.
    pub ordinal: u32,
    /// Deterministically derived iteration seed.
    pub seed: u64,
    /// Whether the result will be discarded as warmup.
    pub warmup: bool,
}

/// Outcome of a scenario invocation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SampleOutcome {
    /// Scenario completed and contributes to latency percentiles.
    Succeeded,
    /// Backend explicitly does not support this configuration.
    Unsupported {
        /// Stable human-readable reason.
        reason: String,
    },
    /// Supported scenario failed.
    Failed {
        /// Bounded diagnostic string.
        error: String,
    },
}

impl SampleOutcome {
    const fn label(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Unsupported { .. } => "unsupported",
            Self::Failed { .. } => "failed",
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Succeeded => "",
            Self::Unsupported { reason } => reason,
            Self::Failed { error } => error,
        }
    }
}

/// Raw measurement returned by a scenario implementation.
#[derive(Clone, Debug, PartialEq)]
pub struct SampleMeasurement {
    /// Wall or CPU duration selected by the scenario, in nanoseconds.
    pub elapsed_ns: u64,
    /// Explicit result classification.
    pub outcome: SampleOutcome,
    /// Bytes read when meaningful.
    pub bytes_read: Option<u64>,
    /// Bytes written when meaningful.
    pub bytes_written: Option<u64>,
    /// Scenario-specific numeric metrics in stable key order.
    pub metrics: BTreeMap<String, f64>,
}

/// A benchmark implementation called for warmup and retained iterations.
pub trait BenchmarkScenario {
    /// Runs one operation and returns its raw observation.
    fn measure(&mut self, iteration: Iteration) -> SampleMeasurement;
}

/// One retained raw sample.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkSample {
    /// Zero-based retained-sample ordinal.
    pub ordinal: u32,
    /// Seed used by this sample.
    pub seed: u64,
    /// Duration in nanoseconds.
    pub elapsed_ns: u64,
    /// Explicit result classification.
    pub outcome: SampleOutcome,
    /// Bytes read when meaningful.
    pub bytes_read: Option<u64>,
    /// Bytes written when meaningful.
    pub bytes_written: Option<u64>,
    /// Scenario-specific numeric metrics.
    pub metrics: BTreeMap<String, f64>,
}

/// Nearest-rank latency summary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PercentileSummary {
    /// Count of successful samples represented.
    pub sample_count: u32,
    /// Minimum value.
    pub min: Option<u64>,
    /// 50th percentile.
    pub p50: Option<u64>,
    /// 95th percentile.
    pub p95: Option<u64>,
    /// 99th percentile.
    pub p99: Option<u64>,
    /// Maximum value.
    pub max: Option<u64>,
}

impl PercentileSummary {
    /// Computes nearest-rank percentiles from an already sorted slice.
    #[must_use]
    pub fn from_sorted(values: &[u64]) -> Self {
        if values.is_empty() {
            return Self {
                sample_count: 0,
                min: None,
                p50: None,
                p95: None,
                p99: None,
                max: None,
            };
        }

        Self {
            sample_count: u32::try_from(values.len()).unwrap_or(u32::MAX),
            min: values.first().copied(),
            p50: nearest_rank(values, 50),
            p95: nearest_rank(values, 95),
            p99: nearest_rank(values, 99),
            max: values.last().copied(),
        }
    }
}

/// Aggregate counts and successful-sample latency percentiles.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchmarkSummary {
    /// Successful retained samples.
    pub succeeded: u32,
    /// Explicitly unsupported retained samples.
    pub unsupported: u32,
    /// Failed retained samples.
    pub failed: u32,
    /// Latency summary over successful samples only.
    pub latency_ns: PercentileSummary,
}

/// Complete authoritative benchmark result.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BenchmarkReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Validated run configuration.
    pub config: BenchmarkConfig,
    /// Environment metadata.
    pub environment: BenchmarkEnvironment,
    /// All retained raw samples, including failures and unsupported cases.
    pub samples: Vec<BenchmarkSample>,
    /// Derived aggregate summary.
    pub summary: BenchmarkSummary,
}

impl BenchmarkReport {
    /// Serializes a stable pretty-printed JSON report.
    ///
    /// # Errors
    ///
    /// Returns an error when scenario-specific floating-point metrics are not finite.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Serializes retained samples as RFC-4180-compatible CSV text.
    ///
    /// # Errors
    ///
    /// Returns an error when scenario-specific floating-point metrics are not finite.
    pub fn to_csv(&self) -> serde_json::Result<String> {
        let mut output = String::from(
            "schema_version,suite,backend,trace_mode,seed,ordinal,sample_seed,status,elapsed_ns,bytes_read,bytes_written,message,metrics_json\n",
        );
        for sample in &self.samples {
            let metrics = serde_json::to_string(&sample.metrics)?;
            let _ = writeln!(
                output,
                "{},{},{},{},{},{},{},{},{},{},{},{},{}",
                self.schema_version,
                csv_field(&self.config.suite),
                csv_field(&self.config.backend),
                self.config.trace_mode.label(),
                self.config.seed,
                sample.ordinal,
                sample.seed,
                sample.outcome.label(),
                sample.elapsed_ns,
                optional_number(sample.bytes_read),
                optional_number(sample.bytes_written),
                csv_field(sample.outcome.message()),
                csv_field(&metrics),
            );
        }
        Ok(output)
    }
}

/// Runs validated benchmark configurations.
pub struct BenchmarkHarness;

impl BenchmarkHarness {
    /// Runs warmups and retained measurements with deterministic per-iteration seeds.
    #[must_use]
    pub fn run<S: BenchmarkScenario>(
        config: BenchmarkConfig,
        environment: BenchmarkEnvironment,
        scenario: &mut S,
    ) -> BenchmarkReport {
        for ordinal in 0..config.warmups {
            let _ = scenario.measure(Iteration {
                ordinal,
                seed: derive_seed(config.seed, u64::from(ordinal)),
                warmup: true,
            });
        }

        let mut samples = Vec::with_capacity(config.repetitions as usize);
        for ordinal in 0..config.repetitions {
            let seed_index = u64::from(config.warmups) + u64::from(ordinal);
            let seed = derive_seed(config.seed, seed_index);
            let measurement = scenario.measure(Iteration {
                ordinal,
                seed,
                warmup: false,
            });
            samples.push(BenchmarkSample {
                ordinal,
                seed,
                elapsed_ns: measurement.elapsed_ns,
                outcome: measurement.outcome,
                bytes_read: measurement.bytes_read,
                bytes_written: measurement.bytes_written,
                metrics: measurement.metrics,
            });
        }

        let summary = summarize(&samples);
        BenchmarkReport {
            schema_version: REPORT_SCHEMA_VERSION,
            config,
            environment,
            samples,
            summary,
        }
    }
}

fn summarize(samples: &[BenchmarkSample]) -> BenchmarkSummary {
    let mut succeeded = 0_u32;
    let mut unsupported = 0_u32;
    let mut failed = 0_u32;
    let mut successful_latency = Vec::new();
    for sample in samples {
        match sample.outcome {
            SampleOutcome::Succeeded => {
                succeeded = succeeded.saturating_add(1);
                successful_latency.push(sample.elapsed_ns);
            }
            SampleOutcome::Unsupported { .. } => unsupported = unsupported.saturating_add(1),
            SampleOutcome::Failed { .. } => failed = failed.saturating_add(1),
        }
    }
    successful_latency.sort_unstable();

    BenchmarkSummary {
        succeeded,
        unsupported,
        failed,
        latency_ns: PercentileSummary::from_sorted(&successful_latency),
    }
}

fn nearest_rank(values: &[u64], percentile: usize) -> Option<u64> {
    let numerator = percentile.saturating_mul(values.len());
    let rank = numerator.div_ceil(100).max(1);
    values.get(rank - 1).copied()
}

const fn derive_seed(root: u64, index: u64) -> u64 {
    let mut value = root.wrapping_add(index.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn optional_number(value: Option<u64>) -> String {
    value.map_or_else(String::new, |number| number.to_string())
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}
