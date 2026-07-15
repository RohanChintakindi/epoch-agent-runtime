#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _, process::Command};

use tempfile::TempDir;

fn workload(fixture: &TempDir, body: &str) -> std::path::PathBuf {
    let script = fixture.path().join("configured-agent.sh");
    fs::write(&script, format!("#!/bin/sh\nset -eu\n{body}"))
        .expect("write configured fixture agent");
    let mut permissions = fs::metadata(&script)
        .expect("configured script metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("make configured fixture executable");
    let manifest = fixture.path().join("configured-workload.toml");
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\nname = \"configured-fixture\"\nexecutable = \"{}\"\n",
            script.display()
        ),
    )
    .expect("write configured workload manifest");
    manifest
}

fn start_record() -> &'static str {
    r#"printf '{"payload":{"agent_id":"cli-fixture","branch_id":"%s","session_id":"%s"},"protocol_version":1,"sequence":0,"type":"agent.start"}\n' "$EPOCH_BRANCH_ID" "$EPOCH_SESSION_ID"
"#
}

fn invoke(fixture: &TempDir, manifest: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(["run", "--manifest"])
        .arg(manifest)
        .output()
        .expect("launch epoch CLI")
}

#[test]
fn run_executes_a_manifest_and_reports_durable_execution_ids() {
    let fixture = TempDir::new().expect("create CLI run fixture");
    let script = fixture.path().join("agent.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '{"payload":{"agent_id":"cli-fixture","branch_id":"%s","session_id":"%s"},"protocol_version":1,"sequence":0,"type":"agent.start"}\n' "$EPOCH_BRANCH_ID" "$EPOCH_SESSION_ID"
printf '{"payload":{"outcome":"succeeded","output_hash":null},"protocol_version":1,"sequence":1,"type":"agent.completion"}\n'
"#,
    )
    .expect("write fixture agent");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("make fixture executable");
    let manifest = fixture.path().join("workload.toml");
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\nname = \"cli-fixture\"\nexecutable = \"{}\"\n",
            script.display()
        ),
    )
    .expect("write workload manifest");

    let output = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(["run", "--manifest"])
        .arg(&manifest)
        .output()
        .expect("launch epoch CLI");

    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("run report is JSON");
    assert_eq!(report["termination"], "succeeded");
    assert_eq!(report["exit_code"], 0);
    assert_eq!(report["protocol_records"], 2);
    assert!(report["session_id"].as_str().is_some());
    assert!(report["branch_id"].as_str().is_some());
    assert!(fixture.path().join(".epoch/state.db").is_file());
}

#[test]
fn run_preserves_a_normal_nonzero_agent_exit() {
    let fixture = TempDir::new().expect("create CLI run fixture");
    let manifest = workload(
        &fixture,
        &format!("{}printf 'agent crashed' >&2\nexit 70\n", start_record()),
    );
    let output = invoke(&fixture, &manifest);

    assert_eq!(output.status.code(), Some(70));
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("nonzero run report is JSON");
    assert_eq!(report["termination"], "nonzero");
    assert_eq!(report["exit_code"], 70);
    assert_eq!(report["stderr_bytes"], 13);
    assert!(output.stderr.is_empty());
}

#[test]
fn run_reserves_exit_125_for_supervisor_failures() {
    let fixture = TempDir::new().expect("create CLI run fixture");
    let manifest = workload(&fixture, "printf 'not-json\\n'\n");
    let output = invoke(&fixture, &manifest);

    assert_eq!(output.status.code(), Some(125));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("supervisor failed"));
    assert!(fixture.path().join(".epoch/state.db").is_file());
}

#[test]
fn explicit_linux_selection_never_falls_back_to_direct_execution() {
    let fixture = TempDir::new().expect("create Linux selection fixture");
    let marker = fixture.path().join("must-not-run");
    let manifest = workload(&fixture, &format!("touch {}\n", marker.display()));
    let output = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(["run", "--backend", "linux", "--manifest"])
        .arg(manifest)
        .output()
        .expect("select Linux backend");

    assert_eq!(output.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Linux execution was selected and cannot start")
    );
    assert!(!marker.exists(), "Linux selection must not launch directly");
    assert!(!fixture.path().join(".epoch").exists());
}
