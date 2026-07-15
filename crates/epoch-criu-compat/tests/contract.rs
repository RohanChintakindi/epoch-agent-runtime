use std::path::{Path, PathBuf};

use epoch_criu_compat::{
    CompatibilityRunner, DiagnosticCode, EvidenceError, RowStatus, RunLimits, RunnerConfig,
    ScalingPlan, Scenario,
};
use tempfile::TempDir;

fn missing_criu_config() -> RunnerConfig {
    RunnerConfig::new(
        PathBuf::from("/definitely/missing/criu"),
        PathBuf::from("/bin/true"),
        RunLimits::new(5_000, 5_000, 64 * 1024).expect("limits"),
        ScalingPlan::new(vec![4 * 1024 * 1024], vec![1]).expect("scaling"),
    )
    .expect("configuration")
}

#[test]
fn declared_scenarios_are_versioned_and_stably_ordered() {
    assert_eq!(
        Scenario::declared(),
        &[
            Scenario::SleepingProcess,
            Scenario::OpenRegularFile,
            Scenario::ProcessTree,
            Scenario::LoopbackSocket,
            Scenario::ExternalTcp,
            Scenario::WorkspaceMutation,
        ]
    );
    assert_eq!(Scenario::SCHEMA_VERSION, 1);
}

#[test]
fn resource_and_scaling_bounds_are_enforced_before_execution() {
    assert!(RunLimits::new(99, 1_000, 1_024).is_err());
    assert!(RunLimits::new(1_000, 120_001, 1_024).is_err());
    assert!(RunLimits::new(1_000, 1_000, 1_023).is_err());
    assert!(RunLimits::new(1_000, 1_000, 1_048_577).is_err());
    assert!(RunLimits::new(1_000, 1_000, 1_024).is_ok());

    assert!(ScalingPlan::new(vec![], vec![1]).is_err());
    assert!(ScalingPlan::new(vec![0], vec![1]).is_err());
    assert!(ScalingPlan::new(vec![1_073_741_825], vec![1]).is_err());
    assert!(ScalingPlan::new(vec![1_048_576], vec![0]).is_err());
    assert!(ScalingPlan::new(vec![1_048_576], vec![65]).is_err());
    assert!(ScalingPlan::new(vec![1_048_576; 17], vec![1]).is_err());
}

#[test]
fn unavailable_criu_preserves_every_row_without_false_success() {
    let evidence = CompatibilityRunner::new(missing_criu_config())
        .run()
        .expect("unsupported report is still a successful experiment run");
    assert_eq!(evidence.report().schema_version, 1);
    assert_eq!(evidence.report().rows.len(), Scenario::declared().len());
    assert!(evidence.report().rows.iter().all(|row| {
        let expected_code = if row.scenario == Scenario::ExternalTcp {
            DiagnosticCode::ExternalTcpUnsupported
        } else {
            DiagnosticCode::CriuUnavailable
        };
        row.status == RowStatus::Unsupported
            && row.diagnostic.code == expected_code
            && row.dump.is_none()
            && row.restore.is_none()
    }));
}

#[test]
fn stable_json_and_markdown_keep_unsupported_rows_and_thresholds() {
    let evidence = CompatibilityRunner::new(missing_criu_config())
        .run()
        .expect("evidence");
    let first = evidence.report().to_stable_json().expect("json");
    let second = evidence.report().to_stable_json().expect("json");
    assert_eq!(first, second);
    for scenario in Scenario::declared() {
        assert!(first.contains(scenario.as_str()));
    }
    let markdown = evidence.report().to_markdown();
    assert!(markdown.contains("| Scenario | Memory bytes | Processes | Status |"));
    assert!(markdown.contains("1000 ms"));
    assert!(markdown.contains("3000 ms"));
    assert!(markdown.contains("narrow_or_kill"));
}

#[test]
fn evidence_writer_never_overwrites_user_data() {
    let evidence = CompatibilityRunner::new(missing_criu_config())
        .run()
        .expect("evidence");
    let parent = TempDir::new().expect("parent");
    let existing = parent.path().join("existing");
    std::fs::create_dir(&existing).expect("existing");
    assert!(matches!(
        evidence.write_new(&existing),
        Err(EvidenceError::OutputExists { .. })
    ));

    let output = parent.path().join("new-evidence");
    evidence.write_new(&output).expect("write evidence");
    assert!(output.join("compatibility.json").is_file());
    assert!(output.join("compatibility.md").is_file());
    assert!(output.join("logs/environment-criu-version.log").is_file());
    assert_private_directory(&output);
}

#[cfg(unix)]
fn assert_private_directory(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;

    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(not(unix))]
fn assert_private_directory(_path: &Path) {}
