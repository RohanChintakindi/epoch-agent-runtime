use std::{collections::BTreeMap, io::Read as _, process::Stdio, time::Instant};

use epoch_sandbox::{
    BackendOutcome, BackendStatus, DirectBackend, ExecutionBackend, LaunchRequest, LinuxBackend,
    ResourceLimits,
};
use serde::Deserialize;

use crate::{
    BackendIsolationReport, BackendLabel, CheckpointInteraction, Diagnostic, IsolationComparison,
    IsolationConfig, IsolationSample, IsolationSummary, SamplePhase, SampleStatus, percentile,
};

#[derive(Deserialize)]
struct ProbeOutput {
    workload_runtime_ns: u64,
    cpu_user_ns: u64,
    cpu_system_ns: u64,
    peak_rss_bytes: u64,
    compatibility: String,
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn run_isolation_comparison(config: &IsolationConfig) -> IsolationComparison {
    let Some(probe) = &config.probe else {
        return unavailable_comparison(
            "fixture_unconfigured",
            "isolation probe path was not configured",
        );
    };
    let Some(helper) = &config.trusted_sandbox_helper else {
        return unavailable_comparison(
            "fixture_unconfigured",
            "trusted epoch-sandbox-init path was not configured",
        );
    };
    let Some(workspace) = &config.workspace else {
        return unavailable_comparison(
            "fixture_unconfigured",
            "isolation workspace path was not configured",
        );
    };
    if config.repetitions < 2 {
        return unavailable_comparison(
            "invalid_repetitions",
            "isolation comparison requires one cold and at least one warm sample",
        );
    }
    let limits = match ResourceLimits::new(
        config.memory_limit_bytes,
        config.pids_limit,
        config.cpu_percent,
    ) {
        Ok(limits) => limits,
        Err(error) => return unavailable_comparison("invalid_limits", error.to_string()),
    };

    let direct_request = match LaunchRequest::new(
        probe,
        ["direct"],
        workspace,
        workspace,
        BTreeMap::new(),
        helper,
        limits,
    ) {
        Ok(request) => request,
        Err(error) => return unavailable_comparison("invalid_fixture", error.to_string()),
    };
    let direct_samples = run_backend(
        &DirectBackend,
        BackendLabel::Direct,
        &direct_request,
        config.repetitions,
    );

    let linux = LinuxBackend::discover();
    if linux.capabilities().status != BackendStatus::Supported {
        let diagnostic = linux.capabilities().diagnostics.first().map_or_else(
            || {
                Diagnostic::new(
                    "linux_backend_unsupported",
                    "discovery returned unsupported",
                )
            },
            |diagnostic| {
                Diagnostic::new(
                    format!("{:?}", diagnostic.code).to_lowercase(),
                    diagnostic.detail.clone(),
                )
            },
        );
        return unsupported_linux_comparison(direct_samples, &diagnostic.code, &diagnostic.detail);
    }

    let linux_request = match LaunchRequest::new(
        probe,
        ["linux"],
        workspace,
        workspace,
        BTreeMap::new(),
        helper,
        limits,
    ) {
        Ok(request) => request,
        Err(error) => {
            return unsupported_linux_comparison(
                direct_samples,
                "linux_request_invalid",
                &error.to_string(),
            );
        }
    };
    let linux_samples = run_backend(
        &linux,
        BackendLabel::Linux,
        &linux_request,
        config.repetitions,
    );
    let direct_checkpoint = CheckpointInteraction::unsupported(
        "the standalone performance probe has no cooperative application checkpoint boundary",
    )
    .for_backend(BackendLabel::Direct);
    let linux_checkpoint = CheckpointInteraction::unsupported(
        "epoch-sandbox is not composed with the CRIU or composite-checkpoint coordinator",
    )
    .for_backend(BackendLabel::Linux);
    let direct = summarize_backend(
        BackendLabel::Direct,
        direct_samples,
        direct_checkpoint.clone(),
    );
    let linux = summarize_backend(BackendLabel::Linux, linux_samples, linux_checkpoint.clone());
    let status = if direct.status == "supported" && linux.status == "supported" {
        "supported"
    } else {
        "failed"
    };
    IsolationComparison {
        status: status.to_owned(),
        direct,
        linux,
        checkpoint_interactions: vec![direct_checkpoint, linux_checkpoint],
    }
}

fn run_backend(
    backend: &dyn ExecutionBackend,
    label: BackendLabel,
    request: &LaunchRequest,
    repetitions: u16,
) -> Vec<IsolationSample> {
    (0..repetitions)
        .map(|ordinal| {
            run_sample(
                backend,
                label,
                request,
                ordinal,
                if ordinal == 0 {
                    SamplePhase::Cold
                } else {
                    SamplePhase::Warm
                },
            )
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn run_sample(
    backend: &dyn ExecutionBackend,
    label: BackendLabel,
    request: &LaunchRequest,
    ordinal: u16,
    phase: SamplePhase,
) -> IsolationSample {
    let started = Instant::now();
    let plan = match backend.prepare(request) {
        BackendOutcome::Supported(plan) => plan,
        BackendOutcome::Unsupported(reason) => {
            return failed_sample(label, phase, ordinal, "backend_unsupported", reason.detail);
        }
    };
    let mut process = match backend.launch(plan, Stdio::piped(), Stdio::piped()) {
        Ok(process) => process,
        Err(error) => {
            return failed_sample(label, phase, ordinal, "launch_failed", error.to_string());
        }
    };
    let mut stdout = Vec::new();
    let stdout_result = process
        .child_mut()
        .stdout
        .take()
        .ok_or_else(|| "probe stdout was not piped".to_owned())
        .and_then(|mut stream| {
            stream
                .read_to_end(&mut stdout)
                .map_err(|error| error.to_string())
        });
    let mut stderr = Vec::new();
    let stderr_result = process
        .child_mut()
        .stderr
        .take()
        .ok_or_else(|| "probe stderr was not piped".to_owned())
        .and_then(|mut stream| {
            stream
                .read_to_end(&mut stderr)
                .map_err(|error| error.to_string())
        });
    let status = process.child_mut().wait();
    let cleanup = backend.cleanup(&mut process);
    let total_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    for result in [stdout_result, stderr_result] {
        if let Err(error) = result {
            return failed_sample(label, phase, ordinal, "output_failed", error);
        }
    }
    if stdout.len() > 64 * 1024 || stderr.len() > 64 * 1024 {
        return failed_sample(
            label,
            phase,
            ordinal,
            "output_oversized",
            "probe output exceeded 64 KiB",
        );
    }
    if let Err(error) = cleanup {
        return failed_sample(label, phase, ordinal, "cleanup_failed", error.to_string());
    }
    match status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            return failed_sample(
                label,
                phase,
                ordinal,
                "probe_failed",
                format!(
                    "exit={:?}; stderr={}",
                    status.code(),
                    String::from_utf8_lossy(&stderr)
                ),
            );
        }
        Err(error) => {
            return failed_sample(label, phase, ordinal, "wait_failed", error.to_string());
        }
    }
    let output: ProbeOutput = match serde_json::from_slice(&stdout) {
        Ok(output) => output,
        Err(error) => {
            return failed_sample(
                label,
                phase,
                ordinal,
                "invalid_probe_json",
                error.to_string(),
            );
        }
    };
    IsolationSample {
        backend: label,
        phase,
        ordinal,
        status: SampleStatus::Supported,
        total_ns,
        launch_overhead_ns: total_ns.saturating_sub(output.workload_runtime_ns),
        workload_runtime_ns: output.workload_runtime_ns,
        cpu_user_ns: output.cpu_user_ns,
        cpu_system_ns: output.cpu_system_ns,
        peak_rss_bytes: output.peak_rss_bytes,
        compatibility: output.compatibility,
        diagnostic: None,
    }
}

fn failed_sample(
    backend: BackendLabel,
    phase: SamplePhase,
    ordinal: u16,
    code: &str,
    detail: impl Into<String>,
) -> IsolationSample {
    IsolationSample {
        backend,
        phase,
        ordinal,
        status: SampleStatus::Failed,
        total_ns: 0,
        launch_overhead_ns: 0,
        workload_runtime_ns: 0,
        cpu_user_ns: 0,
        cpu_system_ns: 0,
        peak_rss_bytes: 0,
        compatibility: "failed".to_owned(),
        diagnostic: Some(Diagnostic::new(code, detail)),
    }
}

#[must_use]
pub fn summarize_backend(
    backend: BackendLabel,
    samples: Vec<IsolationSample>,
    checkpoint_interaction: CheckpointInteraction,
) -> BackendIsolationReport {
    if samples.is_empty()
        || samples
            .iter()
            .any(|sample| sample.status == SampleStatus::Failed)
    {
        return BackendIsolationReport {
            backend,
            status: "failed".to_owned(),
            diagnostic: samples
                .iter()
                .find_map(|sample| sample.diagnostic.clone())
                .or_else(|| Some(Diagnostic::new("no_samples", "backend emitted no samples"))),
            samples,
            summary: None,
            checkpoint_interaction,
        };
    }
    let cold_total_ns = samples
        .iter()
        .find(|sample| sample.phase == SamplePhase::Cold)
        .map_or(0, |sample| sample.total_ns);
    let warm = samples
        .iter()
        .filter(|sample| sample.phase == SamplePhase::Warm)
        .collect::<Vec<_>>();
    let summary = IsolationSummary {
        cold_total_ns,
        warm_total_p50_ns: percentile(warm.iter().map(|sample| sample.total_ns), 50),
        warm_total_p95_ns: percentile(warm.iter().map(|sample| sample.total_ns), 95),
        warm_launch_overhead_p50_ns: percentile(
            warm.iter().map(|sample| sample.launch_overhead_ns),
            50,
        ),
        warm_cpu_p50_ns: percentile(
            warm.iter()
                .map(|sample| sample.cpu_user_ns.saturating_add(sample.cpu_system_ns)),
            50,
        ),
        peak_rss_bytes: samples
            .iter()
            .map(|sample| sample.peak_rss_bytes)
            .max()
            .unwrap_or(0),
    };
    BackendIsolationReport {
        backend,
        status: "supported".to_owned(),
        diagnostic: None,
        samples,
        summary: Some(summary),
        checkpoint_interaction,
    }
}

#[must_use]
pub fn unsupported_linux_comparison(
    direct_samples: Vec<IsolationSample>,
    code: &str,
    detail: &str,
) -> IsolationComparison {
    let direct_checkpoint = CheckpointInteraction::unsupported(
        "the standalone performance probe has no cooperative application checkpoint boundary",
    )
    .for_backend(BackendLabel::Direct);
    let linux_checkpoint = CheckpointInteraction::unsupported(
        "epoch-sandbox is not composed with the CRIU or composite-checkpoint coordinator",
    )
    .for_backend(BackendLabel::Linux);
    IsolationComparison {
        status: "unsupported".to_owned(),
        direct: summarize_backend(
            BackendLabel::Direct,
            direct_samples,
            direct_checkpoint.clone(),
        ),
        linux: BackendIsolationReport {
            backend: BackendLabel::Linux,
            status: "unsupported".to_owned(),
            diagnostic: Some(Diagnostic::new(code, detail)),
            samples: Vec::new(),
            summary: None,
            checkpoint_interaction: linux_checkpoint.clone(),
        },
        checkpoint_interactions: vec![direct_checkpoint, linux_checkpoint],
    }
}

fn unavailable_comparison(code: &str, detail: impl Into<String>) -> IsolationComparison {
    let detail = detail.into();
    let checkpoint_interactions = vec![
        CheckpointInteraction::unsupported(
            "direct performance fixture was unavailable; checkpoint interaction was not measured",
        )
        .for_backend(BackendLabel::Direct),
        CheckpointInteraction::unsupported(
            "Linux performance fixture was unavailable; checkpoint interaction was not measured",
        )
        .for_backend(BackendLabel::Linux),
    ];
    IsolationComparison {
        status: "unsupported".to_owned(),
        direct: BackendIsolationReport {
            backend: BackendLabel::Direct,
            status: "unsupported".to_owned(),
            diagnostic: Some(Diagnostic::new(code, detail.clone())),
            samples: Vec::new(),
            summary: None,
            checkpoint_interaction: checkpoint_interactions[0].clone(),
        },
        linux: BackendIsolationReport {
            backend: BackendLabel::Linux,
            status: "unsupported".to_owned(),
            diagnostic: Some(Diagnostic::new(code, detail)),
            samples: Vec::new(),
            summary: None,
            checkpoint_interaction: checkpoint_interactions[1].clone(),
        },
        checkpoint_interactions,
    }
}
