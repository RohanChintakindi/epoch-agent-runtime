use std::{fs, process::Command};

use serde_json::Value;
use tempfile::TempDir;

#[test]
fn cli_writes_complete_evidence_for_structured_unsupported_host() {
    let parent = TempDir::new().expect("parent");
    let output = parent.path().join("evidence");
    let result = Command::new(env!("CARGO_BIN_EXE_epoch-criu-compat"))
        .args([
            "--output",
            output.to_str().expect("UTF-8 output"),
            "--criu",
            "/definitely/missing/criu",
            "--fixture",
            env!("CARGO_BIN_EXE_epoch-criu-fixture"),
            "--memory-bytes",
            "4194304",
            "--process-counts",
            "2",
        ])
        .output()
        .expect("run CLI");
    assert!(
        result.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(
        &fs::read(output.join("compatibility.json")).expect("compatibility JSON"),
    )
    .expect("JSON");
    assert_eq!(report["schema_version"], 1);
    assert!(report["code_revision"].is_string());
    assert!(report["code_dirty"].is_boolean());
    assert!(
        report["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .all(|row| row["status"] == "unsupported")
    );
}

#[test]
fn cli_refuses_existing_output_without_touching_sentinel() {
    let parent = TempDir::new().expect("parent");
    let output = parent.path().join("existing");
    fs::create_dir(&output).expect("existing");
    fs::write(output.join("sentinel"), b"keep\n").expect("sentinel");
    let result = Command::new(env!("CARGO_BIN_EXE_epoch-criu-compat"))
        .args([
            "--output",
            output.to_str().expect("UTF-8 output"),
            "--criu",
            "/definitely/missing/criu",
            "--fixture",
            env!("CARGO_BIN_EXE_epoch-criu-fixture"),
        ])
        .output()
        .expect("run CLI");
    assert!(!result.status.success());
    assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep\n");
    assert!(!output.join("compatibility.json").exists());
}
