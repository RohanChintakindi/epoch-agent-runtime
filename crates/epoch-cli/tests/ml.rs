#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _, process::Command};

use tempfile::TempDir;

fn completed_run(fixture: &TempDir) -> serde_json::Value {
    let script = fixture.path().join("agent.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '{"payload":{"agent_id":"ml-fixture","branch_id":"%s","session_id":"%s","private":"secret@example.com"},"protocol_version":1,"sequence":0,"type":"agent.start"}\n' "$EPOCH_BRANCH_ID" "$EPOCH_SESSION_ID"
printf '{"payload":{"outcome":"succeeded","output_hash":null},"protocol_version":1,"sequence":1,"type":"agent.completion"}\n'
"#,
    )
    .expect("write fixture agent");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("make agent executable");
    let manifest = fixture.path().join("workload.toml");
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\nname = \"ml-fixture\"\nexecutable = \"{}\"\n",
            script.display()
        ),
    )
    .expect("write workload manifest");
    let output = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(["run", "--manifest"])
        .arg(manifest)
        .output()
        .expect("run fixture");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("run JSON")
}

fn epoch(fixture: &TempDir, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(arguments)
        .output()
        .expect("launch Epoch CLI")
}

#[test]
fn ml_export_writes_private_metadata_only_jsonl_and_summary() {
    let fixture = TempDir::new().expect("temporary CLI fixture");
    let run = completed_run(&fixture);
    let session = run["session_id"].as_str().expect("session ID");
    let output_path = fixture.path().join("trajectory.jsonl");
    let output = epoch(
        &fixture,
        &[
            "ml",
            "export",
            "--session",
            session,
            "--task-group",
            "repo-17.issue-42",
            "--output",
            output_path.to_str().expect("UTF-8 output"),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("export summary JSON");
    assert_eq!(summary["schema_version"], 1);
    assert_eq!(summary["privacy_profile"], "metadata_only");
    assert_eq!(summary["record_count"], 1);
    assert_eq!(summary["labelled_count"], 1);
    assert_eq!(summary["unlabelled_count"], 0);

    let encoded = fs::read_to_string(&output_path).expect("trajectory JSONL");
    let record: serde_json::Value = serde_json::from_str(encoded.trim()).expect("record JSON");
    assert_eq!(record["success_label"], true);
    assert_eq!(record["value_label"], 0.75);
    assert_eq!(record["privacy_profile"], "metadata_only");
    assert!(!record["events"].as_array().expect("events").is_empty());
    for forbidden in [
        session,
        run["branch_id"].as_str().expect("branch ID"),
        "repo-17.issue-42",
        "secret@example.com",
        "agent.completion",
        "branch_state",
        &fixture.path().display().to_string(),
    ] {
        assert!(!encoded.contains(forbidden), "leaked {forbidden:?}");
    }
    let mode = fs::metadata(&output_path)
        .expect("output metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    let duplicate = epoch(
        &fixture,
        &[
            "ml",
            "export",
            "--session",
            session,
            "--task-group",
            "repo-17.issue-42",
            "--output",
            output_path.to_str().expect("UTF-8 output"),
        ],
    );
    assert_eq!(duplicate.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("never overwrites"));
}

#[test]
fn ml_export_rejects_invalid_input_without_creating_state_or_output() {
    let fixture = TempDir::new().expect("temporary CLI error fixture");
    let output_path = fixture.path().join("trajectory.jsonl");
    let missing = epoch(
        &fixture,
        &[
            "ml",
            "export",
            "--session",
            "00000000-0000-4000-8000-000000000000",
            "--task-group",
            "task-1",
            "--output",
            output_path.to_str().expect("UTF-8 output"),
        ],
    );
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("state does not exist"));
    assert!(!fixture.path().join(".epoch").exists());
    assert!(!output_path.exists());

    let run = completed_run(&fixture);
    let session = run["session_id"].as_str().expect("session ID");
    let invalid = epoch(
        &fixture,
        &[
            "ml",
            "export",
            "--session",
            session,
            "--task-group",
            "contains spaces",
            "--output",
            output_path.to_str().expect("UTF-8 output"),
        ],
    );
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("task group"));
    assert!(!output_path.exists());
}
