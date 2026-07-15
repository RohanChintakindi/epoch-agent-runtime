#![cfg(unix)]

use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt as _, process::Command};

use epoch_blob::BlobHash;
use epoch_checkpoint::{APPLICATION_CONTEXT_SCHEMA_VERSION, ApplicationContext, ResumeCursors};
use serde::Serialize;
use tempfile::TempDir;

#[derive(Serialize)]
struct FixtureState {
    seed: u64,
    scenario: &'static str,
    model_response_hash: BlobHash,
    files: BTreeMap<String, BlobHash>,
    memory: Option<serde_json::Value>,
    child: Option<serde_json::Value>,
    network: Option<serde_json::Value>,
    completed_tools: Vec<String>,
}

#[derive(Serialize)]
struct FixtureSummary {
    state: FixtureState,
    state_hash: BlobHash,
    normalized_trace_hash: BlobHash,
    event_count: u64,
    checkpoint_context: ApplicationContext,
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn fixture_manifest(fixture: &TempDir, seed: u64) -> std::path::PathBuf {
    let safe_point_id = format!("safe-point-files-{seed:016x}");
    let state = FixtureState {
        seed,
        scenario: "files",
        model_response_hash: BlobHash::digest(b"recorded model response"),
        files: BTreeMap::new(),
        memory: None,
        child: None,
        network: None,
        completed_tools: Vec::new(),
    };
    let state_hash = BlobHash::digest(&serde_json::to_vec(&state).expect("encode fixture state"));
    let context = ApplicationContext {
        schema_version: APPLICATION_CONTEXT_SCHEMA_VERSION,
        safe_point_id: safe_point_id.clone(),
        deterministic_seed: seed,
        context_revision: 1,
        cursors: ResumeCursors {
            boundary_sequence: 2,
            message_cursor: 2,
            tool_cursor: 0,
            task_cursor: 0,
        },
        model_identifier: "recorded-model-v1".to_owned(),
        tool_registry: BTreeMap::new(),
        messages: Vec::new(),
        pending_tasks: Vec::new(),
        pending_model_request_ids: Vec::new(),
        pending_tool_call_ids: Vec::new(),
        user_visible_summary_hash: None,
    };
    let summary = serde_json::to_string(&FixtureSummary {
        state,
        state_hash: state_hash.clone(),
        normalized_trace_hash: BlobHash::digest(b"fixture trace"),
        event_count: 4,
        checkpoint_context: context,
    })
    .expect("encode captured summary");
    let script = fixture.path().join(format!("recoverable-agent-{seed}.sh"));
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nset -eu\n\
             printf '{{\"payload\":{{\"agent_id\":\"cli-recovery\",\"branch_id\":\"%s\",\"session_id\":\"%s\"}},\"protocol_version\":1,\"sequence\":0,\"type\":\"agent.start\"}}\\n' \"$EPOCH_BRANCH_ID\" \"$EPOCH_SESSION_ID\"\n\
             printf '%s\\n' {}\n\
             printf '%s\\n' {}\n\
             printf '%s\\n' {}\n\
             printf '%s\\n' {} >&2\n",
            shell_quote(&format!(
                "{{\"payload\":{{\"context_hash\":\"{state_hash}\",\"revision\":1}},\"protocol_version\":1,\"sequence\":1,\"type\":\"context.update\"}}"
            )),
            shell_quote(&format!(
                "{{\"payload\":{{\"context_hash\":\"{state_hash}\",\"safe_point_id\":\"{safe_point_id}\"}},\"protocol_version\":1,\"sequence\":2,\"type\":\"safe_point\"}}"
            )),
            shell_quote(&format!(
                "{{\"payload\":{{\"outcome\":\"succeeded\",\"output_hash\":\"{state_hash}\"}},\"protocol_version\":1,\"sequence\":3,\"type\":\"agent.completion\"}}"
            )),
            shell_quote(&summary),
        ),
    )
    .expect("write recoverable agent");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("make script executable");
    let manifest = fixture.path().join(format!("recoverable-{seed}.toml"));
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\nname = \"epoch-test-agent\"\nexecutable = \"{}\"\n",
            script.display()
        ),
    )
    .expect("write recoverable manifest");
    manifest
}

fn epoch(fixture: &TempDir, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(arguments)
        .output()
        .expect("invoke epoch CLI")
}

fn successful_json(output: &std::process::Output, operation: &str) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("{operation} did not return JSON: {error}");
    })
}

fn run_fixture(fixture: &TempDir, seed: u64) -> (String, String) {
    let manifest = fixture_manifest(fixture, seed);
    let run = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(["run", "--manifest"])
        .arg(manifest)
        .output()
        .expect("run fixture");
    let run = successful_json(&run, "run");
    (
        run["session_id"].as_str().expect("session ID").to_owned(),
        run["branch_id"].as_str().expect("branch ID").to_owned(),
    )
}

fn assert_pre_checkpoint_inspection(fixture: &TempDir, session: &str, branch: &str) {
    let status = successful_json(&epoch(fixture, &["status", session]), "status");
    assert_eq!(status["session_id"], session);
    assert_eq!(status["state"], "completed");
    assert_eq!(status["application"]["outcome"], "supported");
    assert!(
        status["application"]["result"]["current_epoch_id"].is_null(),
        "a completed run must not pretend that a checkpoint already exists"
    );

    let events = successful_json(
        &epoch(
            fixture,
            &["events", session, "--branch", branch, "--limit", "100"],
        ),
        "events",
    );
    assert!(
        events["events"]
            .as_array()
            .expect("events array")
            .iter()
            .any(|event| event["kind"] == "process.manifest"),
        "fresh-process inspection must retain the Week 1 process manifest"
    );
}

fn checkpoint(fixture: &TempDir, session: &str, branch: &str) -> String {
    let checkpoint = successful_json(
        &epoch(
            fixture,
            &[
                "checkpoint",
                session,
                "--branch",
                branch,
                "--label",
                "cli-cycle",
            ],
        ),
        "checkpoint",
    );
    assert_eq!(checkpoint["operation"], "checkpoint");
    assert_eq!(checkpoint["outcome"], "supported");
    assert_eq!(checkpoint["result"]["session_id"], session);
    assert_eq!(checkpoint["result"]["branch_id"], branch);
    assert_eq!(checkpoint["result"]["boundary_sequence"], 2);
    assert_eq!(
        checkpoint["result"]["restore_scope"],
        "application_context_only"
    );
    checkpoint["result"]["epoch_id"]
        .as_str()
        .expect("epoch ID")
        .to_owned()
}

fn restore_and_inspect(fixture: &TempDir, session: &str, epoch_id: &str) {
    let restore = successful_json(&epoch(fixture, &["restore", epoch_id]), "restore");
    assert_eq!(restore["operation"], "restore");
    assert_eq!(restore["outcome"], "supported");
    assert_eq!(restore["result"]["activated"], true);
    assert_eq!(restore["result"]["process_restored"], false);
    assert_eq!(restore["result"]["workspace_restored"], false);

    let status = successful_json(&epoch(fixture, &["status", session]), "status");
    assert_eq!(status["session_id"], session);
    assert_eq!(status["state"], "completed");
    assert_eq!(status["application"]["outcome"], "supported");
    assert_eq!(
        status["application"]["result"]["current_epoch_id"],
        epoch_id
    );
    assert_eq!(
        status["application"]["result"]["context"]["cursors"]["boundary_sequence"],
        2
    );
}

fn assert_identical_diff(fixture: &TempDir, epoch_id: &str) {
    let diff = successful_json(
        &epoch(fixture, &["diff", epoch_id, epoch_id, "--json"]),
        "diff",
    );
    assert_eq!(diff["operation"], "diff");
    assert_eq!(diff["outcome"], "supported");
    assert_eq!(diff["result"]["before_epoch_id"], epoch_id);
    assert_eq!(diff["result"]["after_epoch_id"], epoch_id);
    assert_eq!(diff["result"]["diff"]["identical"], true);
    assert_eq!(
        diff["result"]["diff"]["unsupported_sections"][0]["section"],
        "capabilities"
    );
}

#[test]
fn cli_week_two_flow_is_restart_safe_across_three_repetitions() {
    let fixture = TempDir::new().expect("create CLI recovery fixture");
    let mut sessions = std::collections::BTreeSet::new();

    for seed in [73_u64, 74, 75] {
        let (session, branch) = run_fixture(&fixture, seed);
        assert!(
            sessions.insert(session.clone()),
            "session IDs must be fresh"
        );
        assert_pre_checkpoint_inspection(&fixture, &session, &branch);
        let epoch_id = checkpoint(&fixture, &session, &branch);
        restore_and_inspect(&fixture, &session, &epoch_id);
        assert_identical_diff(&fixture, &epoch_id);
    }

    assert_eq!(sessions.len(), 3);
}

#[test]
fn cli_returns_explicit_machine_readable_failed_and_unsupported_outcomes() {
    let fixture = TempDir::new().expect("create CLI outcome fixture");
    let invalid = epoch(&fixture, &["restore", "not-an-epoch-id"]);
    assert!(!invalid.status.success());
    let invalid: serde_json::Value =
        serde_json::from_slice(&invalid.stdout).expect("invalid-ID JSON");
    assert_eq!(invalid["outcome"], "failed");
    assert_eq!(invalid["issue"]["code"], "not_found");

    let future_mode = epoch(
        &fixture,
        &[
            "restore",
            "00000000-0000-0000-0000-000000000001",
            "--mode",
            "fork-on-divergence",
        ],
    );
    assert!(!future_mode.status.success());
    let future_mode: serde_json::Value =
        serde_json::from_slice(&future_mode.stdout).expect("unsupported-mode JSON");
    assert_eq!(future_mode["outcome"], "unsupported");
    assert_eq!(future_mode["issue"]["code"], "unsupported_mode");
}
