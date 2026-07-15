#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _, process::Command};

use tempfile::TempDir;

fn epoch(fixture: &TempDir, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(arguments)
        .output()
        .expect("launch epoch CLI")
}

fn completed_run(fixture: &TempDir) -> serde_json::Value {
    let script = fixture.path().join("agent.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
printf '{"payload":{"agent_id":"inspection-fixture","branch_id":"%s","session_id":"%s"},"protocol_version":1,"sequence":0,"type":"agent.start"}\n' "$EPOCH_BRANCH_ID" "$EPOCH_SESSION_ID"
printf '{"payload":{"outcome":"succeeded","output_hash":null},"protocol_version":1,"sequence":1,"type":"agent.completion"}\n'
"#,
    )
    .expect("write agent fixture");
    let mut permissions = fs::metadata(&script).expect("script metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("make agent executable");
    let manifest = fixture.path().join("workload.toml");
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\nname = \"inspection-fixture\"\nexecutable = \"{}\"\n",
            script.display()
        ),
    )
    .expect("write workload manifest");
    let output = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(["run", "--manifest"])
        .arg(manifest)
        .output()
        .expect("run workload");
    assert!(
        output.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("run report JSON")
}

#[test]
fn fresh_process_queries_durable_status_and_bounded_events() {
    let fixture = TempDir::new().expect("create CLI inspection fixture");
    let run = completed_run(&fixture);
    let session = run["session_id"].as_str().expect("session ID");
    let branch = run["branch_id"].as_str().expect("branch ID");

    let status_output = epoch(&fixture, &["status", session]);
    assert!(
        status_output.status.success(),
        "status failed: {}",
        String::from_utf8_lossy(&status_output.stderr)
    );
    let status: serde_json::Value =
        serde_json::from_slice(&status_output.stdout).expect("status JSON");
    assert_eq!(status["session_id"], session);
    assert_eq!(status["state"], "completed");
    assert_eq!(status["branches"][0]["branch_id"], branch);
    assert_eq!(status["branches"][0]["state"], "completed");

    let first_output = epoch(
        &fixture,
        &["events", session, "--branch", branch, "--limit", "2"],
    );
    assert!(
        first_output.status.success(),
        "events failed: {}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    let first: serde_json::Value =
        serde_json::from_slice(&first_output.stdout).expect("events page JSON");
    assert_eq!(first["offset"], 0);
    assert_eq!(first["limit"], 2);
    assert_eq!(first["events"].as_array().expect("event array").len(), 2);
    assert_eq!(first["has_more"], true);
    assert_eq!(first["next_offset"], 2);

    let rest_output = epoch(
        &fixture,
        &[
            "events", "--offset", "2", "--limit", "100", session, "--branch", branch,
        ],
    );
    assert!(
        rest_output.status.success(),
        "event continuation failed: {}",
        String::from_utf8_lossy(&rest_output.stderr)
    );
    let rest: serde_json::Value =
        serde_json::from_slice(&rest_output.stdout).expect("event continuation JSON");
    assert_eq!(rest["has_more"], false);
    assert!(rest["next_offset"].is_null());

    let all_output = epoch(&fixture, &["events", session, "--limit", "100"]);
    assert!(all_output.status.success());
    let all: serde_json::Value =
        serde_json::from_slice(&all_output.stdout).expect("all events JSON");
    let events = all["events"].as_array().expect("event array");
    let process_manifest = events
        .iter()
        .find(|event| event["kind"] == "process.manifest")
        .expect("durable process manifest event");
    let outcome = process_manifest["payload"]["outcome"]
        .as_str()
        .expect("typed collection outcome");
    if cfg!(target_os = "linux") {
        assert_eq!(outcome, "collected");
        assert_eq!(process_manifest["payload"]["data"]["schema_version"], 1);
        assert!(
            process_manifest["payload"]["data"]["diagnostics"].is_array(),
            "partial collection diagnostics must be preserved"
        );
    } else {
        assert_eq!(outcome, "unsupported");
        assert_eq!(
            process_manifest["payload"]["data"]["metadata"]["required_os"],
            "Linux"
        );
    }
}

#[test]
fn inspection_rejects_bad_ids_unknown_state_and_unbounded_pages() {
    let fixture = TempDir::new().expect("create CLI error fixture");
    let run = completed_run(&fixture);
    let session = run["session_id"].as_str().expect("session ID");

    let malformed = epoch(&fixture, &["status", "not-a-session-id"]);
    assert_eq!(malformed.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("invalid session ID"));

    let unknown = epoch(
        &fixture,
        &["status", "00000000-0000-4000-8000-000000000000"],
    );
    assert_eq!(unknown.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("does not exist"));

    for limit in ["0", "1001"] {
        let output = epoch(&fixture, &["events", session, "--limit", limit]);
        assert_eq!(output.status.code(), Some(2), "limit {limit} must fail");
        assert!(String::from_utf8_lossy(&output.stderr).contains("event limit"));
    }
}

#[test]
fn inspection_distinguishes_missing_and_corrupt_trusted_state() {
    let missing = TempDir::new().expect("create missing-state fixture");
    let output = epoch(
        &missing,
        &["status", "00000000-0000-4000-8000-000000000000"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("state does not exist"));
    assert!(!missing.path().join(".epoch").exists());

    let corrupt = TempDir::new().expect("create corrupt-state fixture");
    fs::create_dir(corrupt.path().join(".epoch")).expect("create state directory");
    fs::write(corrupt.path().join(".epoch/state.db"), b"not a SQLite database")
        .expect("write corrupt database");
    let output = epoch(
        &corrupt,
        &["status", "00000000-0000-4000-8000-000000000000"],
    );
    assert_eq!(output.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&output.stderr).contains("trusted state is unavailable"));
}
