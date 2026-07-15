use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;

use crate::{
    BenchmarkEnvironment, CowMatrixConfig, CowMatrixReport, CowMatrixSummary, CowPointSummary,
    CowResultRow, CowRowKey, CowSample, Diagnostic, HostMemory, PlannedCowRow, PlannedOutcome,
    percentiles,
};

const CHILD_OVERHEAD_BYTES: u64 = 32 * 1024 * 1024;
const RUNNER_OVERHEAD_BYTES: u64 = 64 * 1024 * 1024;

#[must_use]
pub fn plan_cow_matrix(
    config: &CowMatrixConfig,
    memory: HostMemory,
    is_linux: bool,
) -> Vec<PlannedCowRow> {
    let mut allocations = config.allocations_bytes.clone();
    allocations.sort_unstable();
    allocations.dedup();
    let mut fanouts = config.fanouts.clone();
    fanouts.sort_unstable();
    fanouts.dedup();
    let mut dirty_ratios = config.dirty_basis_points.clone();
    dirty_ratios.sort_unstable();
    dirty_ratios.dedup();

    let budget = memory.safety_budget_bytes.min(memory.available_bytes);
    let mut rows = Vec::new();
    for allocation_bytes in allocations {
        for fanout in &fanouts {
            for dirty_basis_points in &dirty_ratios {
                let key = CowRowKey {
                    allocation_bytes,
                    fanout: *fanout,
                    dirty_basis_points: *dirty_basis_points,
                };
                let estimated_peak_bytes = estimated_peak_bytes(key);
                let outcome = if !is_linux {
                    PlannedOutcome::Unsupported {
                        code: "platform_not_linux".to_owned(),
                        detail: "fork/COW PSS evidence requires Linux /proc".to_owned(),
                    }
                } else if estimated_peak_bytes > budget {
                    PlannedOutcome::Skipped {
                        code: "memory_preflight".to_owned(),
                        detail: format!(
                            "estimated peak {estimated_peak_bytes} exceeds safety budget {budget}"
                        ),
                        estimated_peak_bytes,
                    }
                } else {
                    PlannedOutcome::Planned {
                        estimated_peak_bytes,
                    }
                };
                rows.push(PlannedCowRow { key, outcome });
            }
        }
    }
    rows.sort_by_key(|row| row.key);
    rows
}

#[must_use]
pub fn run_cow_matrix(
    config: &CowMatrixConfig,
    environment: &BenchmarkEnvironment,
) -> CowMatrixReport {
    let planned = plan_cow_matrix(config, environment.host_memory, environment.is_linux());
    let rows = planned
        .into_iter()
        .map(|row| run_planned_row(config, environment.host_memory, row))
        .collect::<Vec<_>>();
    CowMatrixReport {
        summary: summarize_matrix(&rows),
        rows,
    }
}

fn estimated_peak_bytes(key: CowRowKey) -> u64 {
    let dirty_bytes = key
        .allocation_bytes
        .saturating_mul(u64::from(key.dirty_basis_points))
        .div_ceil(10_000)
        .saturating_mul(u64::from(key.fanout));
    key.allocation_bytes
        .saturating_mul(2)
        .saturating_add(dirty_bytes)
        .saturating_add(u64::from(key.fanout).saturating_mul(CHILD_OVERHEAD_BYTES))
        .saturating_add(RUNNER_OVERHEAD_BYTES)
}

fn run_planned_row(
    config: &CowMatrixConfig,
    memory: HostMemory,
    row: PlannedCowRow,
) -> CowResultRow {
    match row.outcome {
        PlannedOutcome::Unsupported { code, detail } => CowResultRow {
            key: row.key,
            status: "unsupported".to_owned(),
            estimated_peak_bytes: None,
            diagnostic: Some(Diagnostic::new(code, detail)),
            samples: Vec::new(),
            summary: None,
        },
        PlannedOutcome::Skipped {
            code,
            detail,
            estimated_peak_bytes,
        } => CowResultRow {
            key: row.key,
            status: "skipped".to_owned(),
            estimated_peak_bytes: Some(estimated_peak_bytes),
            diagnostic: Some(Diagnostic::new(code, detail)),
            samples: Vec::new(),
            summary: None,
        },
        PlannedOutcome::Planned {
            estimated_peak_bytes,
        } => execute_row(config, memory, row.key, estimated_peak_bytes),
    }
}

fn execute_row(
    config: &CowMatrixConfig,
    memory: HostMemory,
    key: CowRowKey,
    estimated_peak_bytes: u64,
) -> CowResultRow {
    let Some(helper) = &config.helper else {
        return failed_row(
            key,
            estimated_peak_bytes,
            "helper_unconfigured",
            "COW helper path was not configured",
        );
    };
    if !helper.is_file() || !config.python.is_file() {
        return failed_row(
            key,
            estimated_peak_bytes,
            "helper_unavailable",
            format!(
                "python={} helper={}",
                config.python.display(),
                helper.display()
            ),
        );
    }
    if config.repetitions == 0 {
        return failed_row(
            key,
            estimated_peak_bytes,
            "invalid_repetitions",
            "at least one repetition is required",
        );
    }

    let mut samples = Vec::new();
    for ordinal in 0..config.repetitions {
        match run_helper(config, memory, key, ordinal) {
            Ok(HelperResult::Sample(sample)) => samples.push(sample),
            Ok(HelperResult::Skipped(detail)) => {
                return CowResultRow {
                    key,
                    status: "skipped".to_owned(),
                    estimated_peak_bytes: Some(estimated_peak_bytes),
                    diagnostic: Some(Diagnostic::new("memory_preflight_changed", detail)),
                    samples,
                    summary: None,
                };
            }
            Err(detail) => {
                return CowResultRow {
                    key,
                    status: "failed".to_owned(),
                    estimated_peak_bytes: Some(estimated_peak_bytes),
                    diagnostic: Some(Diagnostic::new("helper_failed", detail)),
                    samples,
                    summary: None,
                };
            }
        }
    }
    CowResultRow {
        key,
        status: "supported".to_owned(),
        estimated_peak_bytes: Some(estimated_peak_bytes),
        summary: Some(summarize_samples(&samples)),
        samples,
        diagnostic: None,
    }
}

enum HelperResult {
    Sample(CowSample),
    Skipped(String),
}

#[derive(Deserialize)]
struct HelperOutput {
    status: String,
    reason: Option<String>,
    runtime_ns: Option<u64>,
    allocation_ns: Option<u64>,
    fork_pause_ns: Option<u64>,
    dirty_ns_max: Option<u64>,
    minor_faults: Option<u64>,
    major_faults: Option<u64>,
    cow_rss_bytes: Option<u64>,
    cow_pss_bytes: Option<u64>,
    full_copy_bytes: Option<u64>,
    full_copy_ns: Option<u64>,
}

fn run_helper(
    config: &CowMatrixConfig,
    memory: HostMemory,
    key: CowRowKey,
    ordinal: u16,
) -> Result<HelperResult, String> {
    let helper = config.helper.as_ref().expect("validated helper");
    let mut child = Command::new(&config.python)
        .arg(helper)
        .arg(key.allocation_bytes.to_string())
        .arg(key.fanout.to_string())
        .arg(key.dirty_basis_points.to_string())
        .arg(memory.safety_budget_bytes.to_string())
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_millis(config.timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("helper timed out after {} ms", config.timeout_ms));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if output.stdout.len() > 64 * 1024 || output.stderr.len() > 64 * 1024 {
        return Err("helper output exceeded 64 KiB".to_owned());
    }
    let parsed: HelperOutput = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "invalid helper JSON: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    if parsed.status == "skipped" {
        return Ok(HelperResult::Skipped(
            parsed
                .reason
                .unwrap_or_else(|| "helper memory preflight changed".to_owned()),
        ));
    }
    if !output.status.success() || parsed.status != "succeeded" {
        return Err(parsed.reason.unwrap_or_else(|| {
            format!(
                "helper exited {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            )
        }));
    }
    let field = |name: &str, value: Option<u64>| {
        value.ok_or_else(|| format!("helper omitted required field {name}"))
    };
    let cow_pss_bytes = field("cow_pss_bytes", parsed.cow_pss_bytes)?;
    let full_copy_bytes = field("full_copy_bytes", parsed.full_copy_bytes)?;
    if full_copy_bytes == 0 {
        return Err("helper reported zero full-copy bytes".to_owned());
    }
    Ok(HelperResult::Sample(CowSample {
        ordinal,
        runtime_ns: field("runtime_ns", parsed.runtime_ns)?,
        allocation_ns: field("allocation_ns", parsed.allocation_ns)?,
        fork_pause_ns: field("fork_pause_ns", parsed.fork_pause_ns)?,
        dirty_ns_max: field("dirty_ns_max", parsed.dirty_ns_max)?,
        minor_faults: field("minor_faults", parsed.minor_faults)?,
        major_faults: field("major_faults", parsed.major_faults)?,
        cow_rss_bytes: field("cow_rss_bytes", parsed.cow_rss_bytes)?,
        cow_pss_bytes,
        full_copy_bytes,
        full_copy_ns: field("full_copy_ns", parsed.full_copy_ns)?,
        pss_to_full_copy_basis_points: cow_pss_bytes
            .saturating_mul(10_000)
            .checked_div(full_copy_bytes)
            .unwrap_or(u64::MAX),
    }))
}

fn summarize_samples(samples: &[CowSample]) -> CowPointSummary {
    CowPointSummary {
        runtime_ns: percentiles(samples.iter().map(|sample| sample.runtime_ns)),
        fork_pause_ns: percentiles(samples.iter().map(|sample| sample.fork_pause_ns)),
        cow_rss_bytes: percentiles(samples.iter().map(|sample| sample.cow_rss_bytes)),
        cow_pss_bytes: percentiles(samples.iter().map(|sample| sample.cow_pss_bytes)),
        minor_faults: percentiles(samples.iter().map(|sample| sample.minor_faults)),
        major_faults: percentiles(samples.iter().map(|sample| sample.major_faults)),
        pss_to_full_copy_basis_points: percentiles(
            samples
                .iter()
                .map(|sample| sample.pss_to_full_copy_basis_points),
        ),
    }
}

fn failed_row(
    key: CowRowKey,
    estimated_peak_bytes: u64,
    code: &str,
    detail: impl Into<String>,
) -> CowResultRow {
    CowResultRow {
        key,
        status: "failed".to_owned(),
        estimated_peak_bytes: Some(estimated_peak_bytes),
        diagnostic: Some(Diagnostic::new(code, detail)),
        samples: Vec::new(),
        summary: None,
    }
}

fn summarize_matrix(rows: &[CowResultRow]) -> CowMatrixSummary {
    CowMatrixSummary {
        total_rows: rows.len(),
        supported_rows: rows.iter().filter(|row| row.status == "supported").count(),
        skipped_rows: rows.iter().filter(|row| row.status == "skipped").count(),
        unsupported_rows: rows
            .iter()
            .filter(|row| row.status == "unsupported")
            .count(),
        failed_rows: rows.iter().filter(|row| row.status == "failed").count(),
    }
}
