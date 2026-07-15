use std::fs;

use epoch_performance_matrix::{
    ArtifactBundle, BenchmarkEnvironment, CowMatrixConfig, HostMemory, IsolationConfig,
    PerformanceConfig, PerformanceRunner, validate_revision, write_artifacts,
};
use tempfile::TempDir;

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

#[test]
fn revision_metadata_is_exact_and_rejects_symbolic_or_dirty_values() {
    assert_eq!(validate_revision(REVISION).unwrap(), REVISION);
    for invalid in ["HEAD", "abc123", "0123456789abcdef0123456789abcdef0123456g"] {
        assert!(validate_revision(invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn mac_like_run_is_structured_and_artifacts_are_stable_and_non_overwriting() {
    let config = PerformanceConfig {
        code_revision: REVISION.to_owned(),
        cow: CowMatrixConfig::required(),
        isolation: IsolationConfig::disabled_fixture(),
    };
    let environment = BenchmarkEnvironment::synthetic_non_linux(
        "macos",
        "aarch64",
        HostMemory {
            available_bytes: 8 * 1024 * 1024 * 1024,
            safety_budget_bytes: 2 * 1024 * 1024 * 1024,
        },
    );
    let report = PerformanceRunner::new(config, environment).run();
    assert_eq!(report.cow.rows.len(), 60);
    assert!(report.cow.rows.iter().all(|row| row.status == "unsupported"));
    assert_eq!(report.isolation.linux.status, "unsupported");
    assert_eq!(report.environment.code_revision, REVISION);

    let directory = TempDir::new().unwrap();
    let output = directory.path().join("evidence");
    let bundle = write_artifacts(&output, &report).unwrap();
    assert_eq!(bundle, ArtifactBundle::at(&output));
    for path in [&bundle.json, &bundle.csv, &bundle.markdown, &bundle.checksums] {
        assert!(path.is_file(), "missing {}", path.display());
    }
    assert!(write_artifacts(&output, &report).is_err());
    let markdown = fs::read_to_string(bundle.markdown).unwrap();
    assert!(markdown.contains(REVISION));
    assert!(markdown.contains("platform_not_linux"));
}

