#[cfg(target_os = "linux")]
use std::{fs, process::Command, time::Instant};

#[cfg(target_os = "linux")]
use serde::Deserialize;

use crate::{BenchmarkEnvironment, SampleOutcome};

#[cfg(target_os = "linux")]
use super::CowSample;
use super::{CowConfig, CowEvidence, bounded};

/// Runs the bounded Linux fork/COW helper or returns a structured unsupported result.
#[must_use]
pub fn run_cow_experiment(config: &CowConfig, environment: BenchmarkEnvironment) -> CowEvidence {
    if let Err(error) = config.validate() {
        return CowEvidence {
            config: config.clone(),
            environment,
            outcome: SampleOutcome::Failed {
                error: bounded(&error.to_string()),
            },
            samples: Vec::new(),
        };
    }
    #[cfg(not(target_os = "linux"))]
    {
        CowEvidence {
            config: config.clone(),
            environment,
            outcome: SampleOutcome::Unsupported {
                reason: "fork COW metrics require Linux /proc/self/smaps_rollup".to_owned(),
            },
            samples: Vec::new(),
        }
    }
    #[cfg(target_os = "linux")]
    {
        run_linux(config, environment)
    }
}

#[cfg(target_os = "linux")]
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum HelperReport {
    Succeeded {
        minor_faults: u64,
        major_faults: u64,
        cow_pss_bytes: u64,
        cow_rss_bytes: u64,
        full_copy_bytes: u64,
        full_copy_ns: u64,
        page_size: u64,
        dirty_pages_per_child: u64,
    },
    Unsupported {
        reason: String,
    },
    Failed {
        error: String,
    },
}

#[cfg(target_os = "linux")]
fn run_linux(config: &CowConfig, environment: BenchmarkEnvironment) -> CowEvidence {
    let temp = match tempfile::TempDir::new() {
        Ok(temp) => temp,
        Err(error) => return failed(config, environment, &error.to_string()),
    };
    let helper = temp.path().join("cow_probe.py");
    if let Err(error) = fs::write(&helper, include_str!("../../helpers/cow_probe.py")) {
        return failed(config, environment, &error.to_string());
    }
    let mut samples = Vec::with_capacity(config.repetitions as usize);
    for ordinal in 0..config.repetitions {
        let started = Instant::now();
        let output = match Command::new("python3")
            .arg(&helper)
            .arg(config.allocation_bytes.to_string())
            .arg(config.child_fanout.to_string())
            .arg(config.dirty_ratio_basis_points.to_string())
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return CowEvidence {
                    config: config.clone(),
                    environment,
                    outcome: SampleOutcome::Unsupported {
                        reason: "python3 COW helper runtime is unavailable".to_owned(),
                    },
                    samples: Vec::new(),
                };
            }
            Err(error) => return failed(config, environment, &error.to_string()),
        };
        let parsed = match serde_json::from_slice::<HelperReport>(&output.stdout) {
            Ok(parsed) => parsed,
            Err(error) => {
                let diagnostic = format!(
                    "{error}; stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                );
                return failed(config, environment, &diagnostic);
            }
        };
        match parsed {
            HelperReport::Succeeded {
                minor_faults,
                major_faults,
                cow_pss_bytes,
                cow_rss_bytes,
                full_copy_bytes,
                full_copy_ns,
                page_size,
                dirty_pages_per_child,
            } => {
                if !output.status.success() || page_size == 0 {
                    return failed(
                        config,
                        environment,
                        "helper success payload disagreed with process status",
                    );
                }
                let expected_dirty_pages = config
                    .allocation_bytes
                    .div_ceil(page_size)
                    .saturating_mul(u64::from(config.dirty_ratio_basis_points))
                    .div_ceil(10_000);
                if dirty_pages_per_child != expected_dirty_pages {
                    return failed(
                        config,
                        environment,
                        "helper dirty-page count disagreed with configuration",
                    );
                }
                samples.push(CowSample {
                    ordinal,
                    elapsed_ns: u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    minor_faults,
                    major_faults,
                    cow_pss_bytes,
                    cow_rss_bytes,
                    full_copy_bytes,
                    full_copy_ns,
                });
            }
            HelperReport::Unsupported { reason } => {
                return CowEvidence {
                    config: config.clone(),
                    environment,
                    outcome: SampleOutcome::Unsupported {
                        reason: bounded(&reason),
                    },
                    samples: Vec::new(),
                };
            }
            HelperReport::Failed { error } => return failed(config, environment, &error),
        }
    }
    CowEvidence {
        config: config.clone(),
        environment,
        outcome: SampleOutcome::Succeeded,
        samples,
    }
}

#[cfg(target_os = "linux")]
fn failed(config: &CowConfig, environment: BenchmarkEnvironment, error: &str) -> CowEvidence {
    CowEvidence {
        config: config.clone(),
        environment,
        outcome: SampleOutcome::Failed {
            error: bounded(error),
        },
        samples: Vec::new(),
    }
}
