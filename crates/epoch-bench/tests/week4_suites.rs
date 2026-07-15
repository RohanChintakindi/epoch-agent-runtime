use std::{collections::BTreeMap, path::Path};

use epoch_bench::{
    BenchmarkEnvironment, CheckpointSuiteConfig, CowConfig, Decision, DecisionThresholds,
    EvidenceKind, SampleOutcome, SuiteName, SuiteRequest, run_checkpoint_suite,
    run_compatibility_matrix, run_cow_experiment, run_fault_matrix, run_suite,
};
use tempfile::TempDir;

fn environment() -> BenchmarkEnvironment {
    BenchmarkEnvironment {
        code_revision: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        code_dirty: false,
        os: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        kernel_release: "test-kernel".to_owned(),
        cpu_model: "test-cpu".to_owned(),
        cpu_count: 4,
        total_memory_bytes: Some(8 * 1024 * 1024 * 1024),
        runtime_version: "rustc test".to_owned(),
        extra: BTreeMap::default(),
    }
}

fn checkpoint_config(root: &Path) -> CheckpointSuiteConfig {
    CheckpointSuiteConfig {
        root: root.to_path_buf(),
        seed: 7,
        warmups: 1,
        repetitions: 2,
        fixture_bytes: 16 * 1024,
        fixture_files: 4,
    }
}

#[test]
fn collects_and_validates_real_environment_metadata() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let collected = BenchmarkEnvironment::collect(&repository).expect("collect environment");

    collected.validate().expect("validated environment");
    assert!(collected.code_revision.len() >= 7);
    assert!(!collected.os.is_empty());
    assert!(!collected.kernel_release.is_empty());
    assert!(!collected.architecture.is_empty());
    assert!(!collected.cpu_model.is_empty());
    assert_ne!(collected.cpu_model, "linux-cpu-model-unreported");
    assert!(collected.cpu_count > 0);
    assert!(!collected.runtime_version.is_empty());
}

#[test]
fn real_checkpoint_suite_separates_trace_modes_and_validates_restores() {
    let temp = TempDir::new().expect("temp");
    let report = run_checkpoint_suite(&checkpoint_config(temp.path()), &environment())
        .expect("checkpoint suite");

    assert_eq!(report.reports.len(), 2);
    assert_ne!(
        report.reports[0].config.trace_mode,
        report.reports[1].config.trace_mode
    );
    for benchmark in &report.reports {
        assert_eq!(benchmark.samples.len(), 2);
        assert_eq!(benchmark.summary.succeeded, 2);
        for sample in &benchmark.samples {
            assert!(sample.bytes_written.is_some_and(|bytes| bytes > 0));
            for metric in [
                "application_capture_ns",
                "workspace_capture_ns",
                "application_restore_ns",
                "workspace_restore_ns",
                "application_checkpoint_bytes",
                "workspace_manifest_bytes",
                "workspace_file_bytes",
            ] {
                assert!(sample.metrics.contains_key(metric), "missing {metric}");
            }
        }
    }
    assert!(report.validation_cases.iter().all(|case| case.passed));
}

#[test]
fn compatibility_matrix_preserves_supported_unsupported_and_failed_rows() {
    let temp = TempDir::new().expect("temp");
    let matrix = run_compatibility_matrix(&checkpoint_config(temp.path()), environment())
        .expect("compatibility matrix");

    assert!(
        matrix
            .rows
            .iter()
            .any(|row| matches!(row.outcome, SampleOutcome::Succeeded))
    );
    assert!(
        matrix
            .rows
            .iter()
            .any(|row| matches!(row.outcome, SampleOutcome::Unsupported { .. }))
    );
    assert!(
        matrix
            .rows
            .iter()
            .any(|row| matches!(row.outcome, SampleOutcome::Failed { .. }))
    );
    assert!(matrix.rows.iter().all(|row| !row.configuration.is_empty()));
    assert!(matrix.rows.iter().all(|row| !row.evidence.is_empty()));
}

#[test]
fn cow_configuration_is_bounded_and_non_linux_is_structured_unsupported() {
    assert!(CowConfig::new(32 * 1024 * 1024, 2, 2_500, 2).is_ok());
    assert!(CowConfig::new(u64::MAX, 2, 2_500, 2).is_err());
    assert!(CowConfig::new(32 * 1024 * 1024, u32::MAX, 2_500, 2).is_err());
    assert!(CowConfig::new(32 * 1024 * 1024, 2, 10_001, 2).is_err());

    if !cfg!(target_os = "linux") {
        let result = run_cow_experiment(
            &CowConfig::new(4 * 1024 * 1024, 1, 2_500, 1).unwrap(),
            environment(),
        );
        assert!(matches!(result.outcome, SampleOutcome::Unsupported { .. }));
        assert!(result.samples.is_empty());
        assert!(result.summary.is_none());
    }
}

#[test]
fn fault_matrix_runs_effect_replay_unknown_and_authority_resurrection_campaigns() {
    let temp = TempDir::new().expect("temp");
    let matrix = run_fault_matrix(temp.path()).expect("fault matrix");

    assert!(
        matrix
            .rows
            .iter()
            .any(|row| row.evidence_kind == EvidenceKind::Actual)
    );
    for stage in [
        "effect_replay_100_runs",
        "effect_unknown_suspends_branch",
        "capability_revocation_resurrection_blocked",
        "capability_policy_rollback_blocked",
    ] {
        let row = matrix
            .rows
            .iter()
            .find(|row| row.stage == stage)
            .unwrap_or_else(|| panic!("missing required fault row {stage}"));
        assert_eq!(row.evidence_kind, EvidenceKind::Actual);
        assert!(matches!(row.outcome, SampleOutcome::Succeeded));
        assert!(row.containment_verified);
    }
    let replay = matrix
        .rows
        .iter()
        .find(|row| row.stage == "effect_replay_100_runs")
        .expect("replay campaign");
    assert_eq!(replay.evidence.get("attempts").map(String::as_str), Some("100"));
    assert_eq!(
        replay.evidence.get("downstream_dispatches").map(String::as_str),
        Some("1")
    );
    assert!(
        matrix
            .rows
            .iter()
            .any(|row| row.evidence_kind == EvidenceKind::Symbolic)
    );
    assert!(
        matrix
            .rows
            .iter()
            .filter(|row| row.evidence_kind == EvidenceKind::Actual)
            .all(|row| {
                matches!(row.outcome, SampleOutcome::Succeeded) && row.containment_verified
            })
    );
    assert!(
        matrix
            .rows
            .iter()
            .filter(|row| row.evidence_kind == EvidenceKind::Symbolic)
            .all(
                |row| matches!(row.outcome, SampleOutcome::Unsupported { .. })
                    && !row.claims_external_exactly_once
            )
    );
}

#[test]
fn all_suite_emits_stable_json_csv_and_threshold_backed_decisions() {
    let temp = TempDir::new().expect("temp");
    let request = SuiteRequest {
        suite: SuiteName::All,
        checkpoint: checkpoint_config(&temp.path().join("checkpoint")),
        cow: CowConfig::new(4 * 1024 * 1024, 1, 2_500, 1).unwrap(),
        thresholds: DecisionThresholds::week4(),
    };
    let report = run_suite(&request, environment()).expect("all suite");

    assert_eq!(report.to_json().unwrap(), report.to_json().unwrap());
    let csv = report.to_csv().unwrap();
    assert!(csv.starts_with("schema_version,run_id,section,case,status"));
    assert!(csv.contains("unsupported"));
    let markdown = report.to_markdown();
    assert!(markdown.contains("## Keep / narrow / kill"));
    assert!(
        report
            .decisions
            .iter()
            .any(|item| item.decision == Decision::Keep)
    );
    assert!(
        report
            .decisions
            .iter()
            .any(|item| item.decision == Decision::Narrow)
    );
    assert!(
        report
            .decisions
            .iter()
            .any(|item| item.decision == Decision::Kill)
    );
    assert!(
        report
            .decisions
            .iter()
            .all(|item| !item.evidence.is_empty())
    );
    let keep = report
        .decisions
        .iter()
        .find(|item| item.decision == Decision::Keep)
        .expect("keep decision");
    assert!(
        keep.evidence
            .iter()
            .any(|fact| fact.starts_with("capture_p95_ns="))
    );
    assert!(
        keep.evidence
            .iter()
            .any(|fact| fact.starts_with("restore_p95_ns="))
    );
}
