#![cfg(target_os = "linux")]

use std::path::PathBuf;

use epoch_criu_compat::{
    CompatibilityRunner, DiagnosticCode, RowStatus, RunLimits, RunnerConfig, ScalingPlan, Scenario,
};
use tempfile::TempDir;

fn enabled() -> bool {
    std::env::var("EPOCH_RUN_PRIVILEGED_CRIU").as_deref() == Ok("1")
}

#[test]
fn real_criu_matrix_preserves_environment_rows_logs_and_verified_success() {
    if !enabled() {
        return;
    }
    let criu = std::env::var_os("EPOCH_CRIU_PATH")
        .map_or_else(|| PathBuf::from("/usr/sbin/criu"), PathBuf::from);
    let config = RunnerConfig::new(
        criu,
        PathBuf::from(env!("CARGO_BIN_EXE_epoch-criu-fixture")),
        RunLimits::new(30_000, 30_000, 256 * 1024).expect("limits"),
        ScalingPlan::new(vec![4 * 1024 * 1024], vec![1]).expect("scaling"),
    )
    .expect("configuration");
    let evidence = CompatibilityRunner::new(config).run().expect("matrix run");
    let report = evidence.report();

    assert_eq!(report.rows.len(), Scenario::declared().len());
    assert!(
        report
            .environment
            .kernel_release
            .as_deref()
            .is_some_and(|v| !v.is_empty())
    );
    assert!(
        report
            .environment
            .criu_version
            .as_deref()
            .is_some_and(|v| !v.is_empty())
    );
    assert_ne!(
        report.environment.criu_check_diagnostic.code,
        DiagnosticCode::CriuUnavailable
    );
    let external = report
        .rows
        .iter()
        .find(|row| row.scenario == Scenario::ExternalTcp)
        .expect("external TCP row");
    assert_eq!(external.status, RowStatus::Unsupported);
    assert_eq!(
        external.diagnostic.code,
        DiagnosticCode::ExternalTcpUnsupported
    );
    assert!(external.dump.is_none() && external.restore.is_none());
    for row in report
        .rows
        .iter()
        .filter(|row| row.status == RowStatus::Supported)
    {
        assert!(row.dump.as_ref().is_some_and(|dump| !dump.timed_out));
        assert!(
            row.restore
                .as_ref()
                .is_some_and(|restore| !restore.timed_out)
        );
        assert!(row.image_bytes.is_some_and(|bytes| bytes > 0));
        assert!(row.restored_behavior_verified);
    }

    let parent = TempDir::new().expect("evidence parent");
    let output = parent.path().join("native-evidence");
    evidence.write_new(&output).expect("persist evidence");
    for row in &report.rows {
        assert!(
            output.join(&row.diagnostic.log_artifact).is_file(),
            "missing log for {:?}",
            row.scenario
        );
    }
}
