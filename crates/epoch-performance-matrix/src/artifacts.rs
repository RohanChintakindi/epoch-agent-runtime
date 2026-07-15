use std::{fmt::Write as _, fs, path::Path};

use sha2::{Digest as _, Sha256};

use crate::{ArtifactBundle, MatrixError, PerformanceReport};

/// Writes canonical JSON, flat CSV, Markdown, and SHA-256 evidence into a new directory.
///
/// # Errors
///
/// Refuses to overwrite an existing path and preserves serialization/I/O failures.
pub fn write_artifacts(
    output: &Path,
    report: &PerformanceReport,
) -> Result<ArtifactBundle, MatrixError> {
    if output.exists() {
        return Err(MatrixError::OutputExists(output.to_path_buf()));
    }
    let Some(parent) = output.parent().filter(|parent| parent.is_dir()) else {
        return Err(MatrixError::MissingOutputParent(output.to_path_buf()));
    };
    let staging = tempfile::Builder::new()
        .prefix(".epoch-performance-")
        .tempdir_in(parent)?;
    let staging_bundle = ArtifactBundle::at(staging.path());
    let mut json = serde_json::to_string_pretty(report)?;
    json.push('\n');
    let csv = render_csv(report);
    let markdown = render_markdown(report);
    fs::write(&staging_bundle.json, json.as_bytes())?;
    fs::write(&staging_bundle.csv, csv.as_bytes())?;
    fs::write(&staging_bundle.markdown, markdown.as_bytes())?;
    let checksums = [
        ("report.json", digest(json.as_bytes())),
        ("samples.csv", digest(csv.as_bytes())),
        ("RESULTS.md", digest(markdown.as_bytes())),
    ]
    .into_iter()
    .fold(String::new(), |mut output, (name, hash)| {
        writeln!(output, "{hash}  {name}").expect("write string");
        output
    });
    fs::write(&staging_bundle.checksums, checksums)?;
    fs::rename(staging.path(), output)?;
    Ok(ArtifactBundle::at(output))
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut output, byte| {
            write!(output, "{byte:02x}").expect("write string");
            output
        })
}

#[allow(clippy::too_many_lines)]
fn render_csv(report: &PerformanceReport) -> String {
    let mut output = "section,case,status,ordinal,phase,runtime_ns,pause_or_overhead_ns,cpu_user_ns,cpu_system_ns,rss_bytes,pss_bytes,minor_faults,major_faults,full_copy_bytes,ratio_basis_points,diagnostic\n".to_owned();
    for row in &report.cow.rows {
        let case = format!(
            "{}-{}-{}",
            row.key.allocation_bytes, row.key.fanout, row.key.dirty_basis_points
        );
        if row.samples.is_empty() {
            csv_line(
                &mut output,
                &[
                    "cow",
                    &case,
                    &row.status,
                    "",
                    "",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    &diagnostic(row.diagnostic.as_ref()),
                ],
            );
        }
        for sample in &row.samples {
            csv_line(
                &mut output,
                &[
                    "cow",
                    &case,
                    &row.status,
                    &sample.ordinal.to_string(),
                    "retained",
                    &sample.runtime_ns.to_string(),
                    &sample.fork_pause_ns.to_string(),
                    "0",
                    "0",
                    &sample.cow_rss_bytes.to_string(),
                    &sample.cow_pss_bytes.to_string(),
                    &sample.minor_faults.to_string(),
                    &sample.major_faults.to_string(),
                    &sample.full_copy_bytes.to_string(),
                    &sample.pss_to_full_copy_basis_points.to_string(),
                    "",
                ],
            );
        }
    }
    for backend in [&report.isolation.direct, &report.isolation.linux] {
        let case = format!("{:?}", backend.backend).to_lowercase();
        if backend.samples.is_empty() {
            csv_line(
                &mut output,
                &[
                    "isolation",
                    &case,
                    &backend.status,
                    "",
                    "",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    &diagnostic(backend.diagnostic.as_ref()),
                ],
            );
        }
        for sample in &backend.samples {
            csv_line(
                &mut output,
                &[
                    "isolation",
                    &case,
                    if sample.status == crate::SampleStatus::Supported {
                        "supported"
                    } else {
                        "failed"
                    },
                    &sample.ordinal.to_string(),
                    &format!("{:?}", sample.phase).to_lowercase(),
                    &sample.workload_runtime_ns.to_string(),
                    &sample.launch_overhead_ns.to_string(),
                    &sample.cpu_user_ns.to_string(),
                    &sample.cpu_system_ns.to_string(),
                    &sample.peak_rss_bytes.to_string(),
                    "0",
                    "0",
                    "0",
                    "0",
                    "0",
                    &diagnostic(sample.diagnostic.as_ref()),
                ],
            );
        }
    }
    output
}

fn csv_line(output: &mut String, values: &[&str]) {
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push('"');
        output.push_str(&value.replace('"', "\"\""));
        output.push('"');
    }
    output.push('\n');
}

fn diagnostic(value: Option<&crate::Diagnostic>) -> String {
    value.map_or_else(String::new, |value| {
        format!("{}: {}", value.code, value.detail)
    })
}

fn render_markdown(report: &PerformanceReport) -> String {
    let mut output = format!(
        "# Final performance matrix\n\nRevision `{}` on {} {} / kernel `{}`. Safety budget: {} bytes.\n\n## COW matrix\n\nRows: {} total, {} supported, {} skipped, {} unsupported, {} failed.\n\n| Allocation | Fan-out | Dirty bps | Status | Runtime p50 ns | Fork pause p95 ns | PSS/full-copy p50 bps | Diagnostic |\n|---:|---:|---:|---|---:|---:|---:|---|\n",
        report.environment.code_revision,
        report.environment.operating_system,
        report.environment.architecture,
        report.environment.kernel_release,
        report.environment.host_memory.safety_budget_bytes,
        report.cow.summary.total_rows,
        report.cow.summary.supported_rows,
        report.cow.summary.skipped_rows,
        report.cow.summary.unsupported_rows,
        report.cow.summary.failed_rows,
    );
    for row in &report.cow.rows {
        let summary = row.summary.as_ref();
        writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            row.key.allocation_bytes,
            row.key.fanout,
            row.key.dirty_basis_points,
            row.status,
            summary.map_or(0, |value| value.runtime_ns.p50),
            summary.map_or(0, |value| value.fork_pause_ns.p95),
            summary.map_or(0, |value| value.pss_to_full_copy_basis_points.p50),
            diagnostic(row.diagnostic.as_ref()).replace('|', "\\|"),
        )
        .expect("write string");
    }
    output.push_str("\n## Isolation comparison\n\n");
    output.push_str("| Backend | Status | Cold total ns | Warm total p50 ns | Warm launch overhead p50 ns | Warm CPU p50 ns | Peak RSS bytes | Checkpoint interaction |\n|---|---|---:|---:|---:|---:|---:|---|\n");
    for backend in [&report.isolation.direct, &report.isolation.linux] {
        let summary = backend.summary.as_ref();
        writeln!(
            output,
            "| {:?} | {} | {} | {} | {} | {} | {} | {}: {} |",
            backend.backend,
            backend.status,
            summary.map_or(0, |value| value.cold_total_ns),
            summary.map_or(0, |value| value.warm_total_p50_ns),
            summary.map_or(0, |value| value.warm_launch_overhead_p50_ns),
            summary.map_or(0, |value| value.warm_cpu_p50_ns),
            summary.map_or(0, |value| value.peak_rss_bytes),
            backend.checkpoint_interaction.status,
            backend.checkpoint_interaction.detail.replace('|', "\\|"),
        )
        .expect("write string");
    }
    output
}
