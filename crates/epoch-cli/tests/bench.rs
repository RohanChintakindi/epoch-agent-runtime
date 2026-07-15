use std::{fs, process::Command};

use tempfile::TempDir;

fn run_checkpoint(root: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(root.parent().expect("benchmark root parent"))
        .args([
            "bench",
            "run",
            "checkpoint",
            "--root",
            root.to_str().expect("UTF-8 root"),
            "--warmups",
            "0",
            "--repetitions",
            "1",
            "--fixture-bytes",
            "4096",
            "--fixture-files",
            "2",
        ])
        .output()
        .expect("run checkpoint benchmark")
}

#[test]
fn bench_run_persists_every_stable_artifact_and_report_reloads_it() {
    let temp = TempDir::new().expect("temp");
    let root = temp.path().join("benchmarks");
    let output = run_checkpoint(&root);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&output.stdout).expect("run summary");
    assert_eq!(summary["suite"], "checkpoint");
    assert_eq!(summary["status"], "completed");
    let run_id = summary["run_id"].as_str().expect("run ID");
    let run_root = root.join(run_id);
    for artifact in ["report.json", "samples.csv", "RESULTS.md"] {
        assert!(run_root.join(artifact).is_file(), "missing {artifact}");
    }

    let report = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args([
            "bench",
            "report",
            run_id,
            "--root",
            root.to_str().unwrap(),
            "--format",
            "json",
        ])
        .output()
        .expect("reload report");
    assert!(report.status.success());
    let reloaded: serde_json::Value = serde_json::from_slice(&report.stdout).expect("report JSON");
    assert_eq!(reloaded["run_id"], run_id);
    assert_eq!(reloaded["suite"], "checkpoint");
    assert_eq!(reloaded["checkpoint"]["reports"].as_array().unwrap().len(), 2);
}

#[test]
fn bench_report_supports_csv_and_markdown_without_recomputing() {
    let temp = TempDir::new().expect("temp");
    let root = temp.path().join("benchmarks");
    let output = run_checkpoint(&root);
    let summary: serde_json::Value = serde_json::from_slice(&output.stdout).expect("run summary");
    let run_id = summary["run_id"].as_str().expect("run ID");
    for (format, prefix) in [
        ("csv", "schema_version,run_id,section,case,status"),
        ("markdown", "# Epoch benchmark report"),
    ] {
        let report = Command::new(env!("CARGO_BIN_EXE_epoch"))
            .args([
                "bench",
                "report",
                run_id,
                "--root",
                root.to_str().unwrap(),
                "--format",
                format,
            ])
            .output()
            .expect("reload report");
        assert!(report.status.success());
        assert!(String::from_utf8_lossy(&report.stdout).starts_with(prefix));
    }
}

#[test]
fn bench_rejects_unknown_suite_and_report_path_traversal_without_creating_artifacts() {
    let temp = TempDir::new().expect("temp");
    let root = temp.path().join("benchmarks");
    let unknown = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args([
            "bench",
            "run",
            "not-a-suite",
            "--root",
            root.to_str().unwrap(),
        ])
        .output()
        .expect("reject suite");
    assert!(!unknown.status.success());
    assert!(!root.exists());

    fs::create_dir(&root).expect("benchmark root");
    let traversal = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args([
            "bench",
            "report",
            "../../outside",
            "--root",
            root.to_str().unwrap(),
        ])
        .output()
        .expect("reject traversal");
    assert!(!traversal.status.success());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}
