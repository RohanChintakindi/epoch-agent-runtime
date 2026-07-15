use epoch_performance_matrix::{
    BackendLabel, CheckpointInteraction, IsolationSample, SamplePhase, SampleStatus,
    summarize_backend, unsupported_linux_comparison,
};

fn sample(phase: SamplePhase, ordinal: u16, total_ns: u64) -> IsolationSample {
    IsolationSample {
        backend: BackendLabel::Direct,
        phase,
        ordinal,
        status: SampleStatus::Supported,
        total_ns,
        launch_overhead_ns: total_ns - 10,
        workload_runtime_ns: 10,
        cpu_user_ns: 4,
        cpu_system_ns: 2,
        peak_rss_bytes: 4096,
        compatibility: "completed".to_owned(),
        diagnostic: None,
    }
}

#[test]
fn isolation_summary_keeps_cold_separate_from_warm_percentiles() {
    let samples = vec![
        sample(SamplePhase::Cold, 0, 100),
        sample(SamplePhase::Warm, 1, 20),
        sample(SamplePhase::Warm, 2, 40),
        sample(SamplePhase::Warm, 3, 30),
    ];
    let summary = summarize_backend(
        BackendLabel::Direct,
        samples,
        CheckpointInteraction::unsupported("not composed in performance fixture"),
    );
    assert_eq!(summary.status, "supported");
    assert_eq!(summary.summary.as_ref().unwrap().cold_total_ns, 100);
    assert_eq!(summary.summary.as_ref().unwrap().warm_total_p50_ns, 30);
    assert_eq!(summary.summary.as_ref().unwrap().warm_total_p95_ns, 40);
    assert_eq!(summary.summary.as_ref().unwrap().peak_rss_bytes, 4096);
}

#[test]
fn unsupported_linux_is_explicit_and_never_populated_with_direct_samples() {
    let comparison = unsupported_linux_comparison(
        vec![sample(SamplePhase::Cold, 0, 100)],
        "bubblewrap_missing",
        "Bubblewrap was not discovered",
    );
    assert_eq!(comparison.direct.status, "supported");
    assert_eq!(comparison.linux.status, "unsupported");
    assert!(comparison.linux.samples.is_empty());
    assert_eq!(comparison.linux.diagnostic.as_ref().unwrap().code, "bubblewrap_missing");
    assert_eq!(comparison.status, "unsupported");
    assert_eq!(comparison.checkpoint_interactions.len(), 2);
}

