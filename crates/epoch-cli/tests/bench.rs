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

fn run_all(root: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(root.parent().expect("benchmark root parent"))
        .args([
            "bench",
            "run",
            "all",
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
            "--cow-allocation-bytes",
            "4194304",
            "--cow-children",
            "1",
            "--cow-repetitions",
            "1",
            "--performance-repetitions",
            "1",
            "--isolation-repetitions",
            "2",
            "--performance-max-memory-bytes",
            "1",
        ])
        .output()
        .expect("run all benchmark")
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
    assert_eq!(
        reloaded["checkpoint"]["reports"].as_array().unwrap().len(),
        2
    );
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
fn bench_run_all_embeds_the_final_60_row_performance_campaign() {
    let temp = TempDir::new().expect("temp");
    let root = temp.path().join("benchmarks");
    let output = run_all(&root);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&output.stdout).expect("run summary");
    let run_id = summary["run_id"].as_str().expect("run ID");
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join(run_id).join("report.json")).expect("report JSON"),
    )
    .expect("parse report JSON");
    assert_eq!(report["schema_version"], 2);
    assert_eq!(
        report["performance"]["cow"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        60
    );
    assert!(report["performance"]["isolation"].is_object());

    let csv = fs::read_to_string(root.join(run_id).join("samples.csv")).expect("samples CSV");
    assert!(csv.contains("final_cow"));
    assert!(csv.contains("final_isolation"));
    let markdown =
        fs::read_to_string(root.join(run_id).join("RESULTS.md")).expect("results Markdown");
    assert!(markdown.contains("## Final performance matrix"));
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
