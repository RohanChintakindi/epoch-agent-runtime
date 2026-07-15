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

fn captured_fixture(seed: u64) -> (BlobHash, String, String) {
    let safe_point_id = format!("safe-point-files-{seed:016x}");
    let state = FixtureState {
        seed,
        scenario: "files",
        model_response_hash: BlobHash::digest(format!("model-{seed}").as_bytes()),
        files: BTreeMap::new(),
        memory: None,
        child: None,
        network: None,
        completed_tools: Vec::new(),
    };
    let state_hash = BlobHash::digest(&serde_json::to_vec(&state).expect("encode state"));
    let summary = serde_json::to_string(&FixtureSummary {
        state,
        state_hash: state_hash.clone(),
        normalized_trace_hash: BlobHash::digest(format!("trace-{seed}").as_bytes()),
        event_count: 4,
        checkpoint_context: ApplicationContext {
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
        },
    })
    .expect("encode summary");
    (state_hash, safe_point_id, summary)
}

fn case_body(seed: u64) -> String {
    let (state_hash, safe_point_id, summary) = captured_fixture(seed);
    format!(
        "{seed})\n\
         printf '%s\\n' {}\n\
         printf '%s\\n' {}\n\
         printf '%s\\n' {}\n\
         printf '%s\\n' {} >&2\n\
         ;;",
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
    )
}

fn executable(fixture: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = fixture.path().join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}")).expect("write fixture executable");
    let mut permissions = fs::metadata(&path).expect("fixture metadata").permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).expect("make fixture executable");
    path
}

fn deterministic_agent(fixture: &TempDir) -> std::path::PathBuf {
    executable(
        fixture,
        "demo-agent.sh",
        &format!(
            "seed=''\n\
             workspace=''\n\
             while [ \"$#\" -gt 0 ]; do\n\
               case \"$1\" in\n\
                 --seed) seed=$2; shift 2 ;;\n\
                 --workspace) workspace=$2; shift 2 ;;\n\
                 *) shift ;;\n\
               esac\n\
             done\n\
             mkdir -p \"$workspace\"\n\
             printf 'created-by-agent-%s' \"$seed\" > \"$workspace/artifact.txt\"\n\
             printf '{{\"payload\":{{\"agent_id\":\"demo-fixture\",\"branch_id\":\"%s\",\"session_id\":\"%s\"}},\"protocol_version\":1,\"sequence\":0,\"type\":\"agent.start\"}}\\n' \"$EPOCH_BRANCH_ID\" \"$EPOCH_SESSION_ID\"\n\
             case \"$seed\" in\n\
               {}\n\
               {}\n\
               *) exit 64 ;;\n\
             esac\n",
            case_body(424_242),
            case_body(424_243),
        ),
    )
}

fn demo(
    agent: &std::path::Path,
    root: &std::path::Path,
    workspace: &std::path::Path,
) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args(["demo", "--agent"])
        .arg(agent)
        .arg("--root")
        .arg(root)
        .arg("--workspace")
        .arg(workspace)
        .arg("--json")
        .output()
        .expect("run epoch demo")
}

fn phase<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    report["phases"]
        .as_array()
        .expect("phase array")
        .iter()
        .find(|phase| phase["name"] == name)
        .unwrap_or_else(|| panic!("missing phase {name}"))
}

#[test]
fn demo_runs_real_composite_recovery_flow_and_reports_only_current_gaps() {
    let fixture = TempDir::new().expect("create demo fixture");
    let agent = deterministic_agent(&fixture);
    let root = fixture.path().join("demo-root");
    let workspace = root.join("workspaces");

    let output = demo(&agent, &root, &workspace);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("demo JSON report");
    assert_eq!(report["schema_version"], 1);
    assert!(
        report["code_revision"] == "unavailable"
            || report["code_revision"]
                .as_str()
                .is_some_and(|value| value.len() == 40)
    );
    assert!(report["code_dirty"].is_boolean());
    assert_eq!(report["outcome"], "completed_with_unsupported");
    assert!(
        report["summary"]
            .as_str()
            .expect("summary")
            .contains("13/13")
    );
    assert_eq!(report["phases"].as_array().expect("phases").len(), 13);
    assert!(
        report["phases"]
            .as_array()
            .expect("phases")
            .iter()
            .all(|phase| phase["status"] == "succeeded")
    );
    assert!(phase(&report, "run_baseline")["evidence"]["session_id"].is_string());
    assert!(phase(&report, "checkpoint_baseline")["evidence"]["epoch_id"].is_string());
    assert_eq!(
        phase(&report, "restore_baseline")["evidence"]["workspace_restored"],
        true
    );
    assert!(phase(&report, "restore_baseline")["evidence"]["workspace_target"].is_string());
    assert_eq!(
        phase(&report, "status_after_restore")["evidence"]["original_workspace_change_preserved"],
        true
    );
    assert_eq!(
        phase(&report, "status_after_restore")["evidence"]["restored_workspace_matches_checkpoint"],
        true
    );
    assert_eq!(
        phase(&report, "semantic_diff")["evidence"]["identical"],
        false
    );
    assert!(phase(&report, "semantic_diff")["evidence"]["workspace"]["identical"].is_boolean());
    assert!(
        phase(&report, "semantic_diff")["evidence"]["capabilities"]["before_frontier"].is_number()
    );
    assert!(phase(&report, "semantic_diff")["evidence"]["effects"]["before_frontier"].is_number());
    assert!(phase(&report, "fork")["evidence"]["branch_id"].is_string());
    assert_eq!(
        report["unsupported_sections"]
            .as_array()
            .expect("unsupported sections")
            .iter()
            .map(|section| section["section"].as_str().expect("section"))
            .collect::<Vec<_>>(),
        ["continuation", "effects", "isolation", "process"]
    );
    let report_path = report["report_path"].as_str().expect("report path");
    assert!(std::path::Path::new(report_path).is_file());

    let rerun = demo(&agent, &root, &workspace);
    assert!(rerun.status.success());
    let rerun: serde_json::Value =
        serde_json::from_slice(&rerun.stdout).expect("rerun JSON report");
    assert_ne!(rerun["run_root"], report["run_root"]);
    assert!(std::path::Path::new(report_path).is_file());
}

#[test]
fn demo_refuses_unowned_or_external_targets_without_mutating_them() {
    let fixture = TempDir::new().expect("create unsafe-root fixture");
    let agent = deterministic_agent(&fixture);
    let root = fixture.path().join("user-data");
    fs::create_dir(&root).expect("create user directory");
    fs::write(root.join("important.txt"), b"do not modify").expect("write user data");

    let output = demo(&agent, &root, &root.join("workspaces"));
    assert!(!output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("unsafe-root JSON");
    assert_eq!(report["outcome"], "failed");
    assert_eq!(report["failure"]["code"], "unsafe_demo_root");
    assert_eq!(
        fs::read(root.join("important.txt")).expect("read user data"),
        b"do not modify"
    );
    assert!(!root.join(".epoch-demo-owned.json").exists());

    let safe_root = fixture.path().join("safe-root");
    let external = fixture.path().join("external-workspace");
    let output = demo(&agent, &safe_root, &external);
    assert!(!output.status.success());
    assert!(!safe_root.exists());
    assert!(!external.exists());
}

#[test]
fn demo_failure_preserves_bounded_diagnostics_and_partial_evidence() {
    let fixture = TempDir::new().expect("create failed-demo fixture");
    let agent = executable(
        &fixture,
        "failing-agent.sh",
        "printf 'agent failed' >&2\nexit 9\n",
    );
    let root = fixture.path().join("failed-root");
    let output = demo(&agent, &root, &root.join("workspaces"));
    assert!(!output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("failed demo JSON");
    assert_eq!(report["outcome"], "failed");
    assert_eq!(report["phases"][0]["name"], "doctor");
    assert_eq!(report["phases"][0]["status"], "succeeded");
    assert_eq!(report["phases"][1]["name"], "run_baseline");
    assert_eq!(report["phases"][1]["status"], "failed");
    assert!(
        report["failure"]["diagnostic"]
            .as_str()
            .expect("diagnostic")
            .len()
            <= 4096
    );
    assert!(std::path::Path::new(report["report_path"].as_str().expect("report path")).is_file());
}
