use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use epoch_protocol::{Message, decode_line};

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let suffix = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("epoch-agent-cli-{}-{suffix}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("test workspace should be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn invoke(workspace: &Path, extra_args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch-test-agent"))
        .args(["--workspace", workspace.to_str().expect("UTF-8 test path")])
        .args(extra_args)
        .output()
        .expect("test agent binary should launch")
}

#[test]
fn cli_selects_scenario_and_emits_machine_readable_summary() {
    let workspace = TestDir::new();
    let output = invoke(workspace.path(), &["--scenario", "files", "--seed", "99"]);

    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tools: Vec<_> = String::from_utf8(output.stdout)
        .expect("trace should be UTF-8")
        .lines()
        .filter_map(|line| {
            match decode_line(line.as_bytes())
                .expect("valid boundary message")
                .message
            {
                Message::ToolCall(call) => Some(call.tool),
                _ => None,
            }
        })
        .collect();
    assert_eq!(tools, ["file.create", "file.append"]);

    let summary: serde_json::Value =
        serde_json::from_slice(&output.stderr).expect("stderr should contain one JSON summary");
    assert_eq!(summary["state"]["seed"], 99);
    assert_eq!(summary["state"]["scenario"], "files");
    assert!(summary["state_hash"].as_str().is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }));
    assert!(summary["normalized_trace_hash"].as_str().is_some());
}

#[test]
fn cli_crash_point_exits_nonzero_after_flushing_partial_trace() {
    let workspace = TestDir::new();
    let output = invoke(
        workspace.path(),
        &[
            "--scenario",
            "full",
            "--seed",
            "99",
            "--crash-at",
            "after-model",
        ],
    );

    assert_eq!(output.status.code(), Some(70));
    let kinds: Vec<_> = String::from_utf8(output.stdout)
        .expect("trace should be UTF-8")
        .lines()
        .map(|line| {
            decode_line(line.as_bytes())
                .expect("valid partial trace")
                .message
                .kind()
        })
        .collect();
    assert_eq!(kinds.last(), Some(&"model.response"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("injected crash"));
}

#[test]
fn cli_uses_the_supervisors_trusted_execution_binding() {
    let workspace = TestDir::new();
    let output = Command::new(env!("CARGO_BIN_EXE_epoch-test-agent"))
        .args([
            "--workspace",
            workspace.path().to_str().expect("UTF-8 test path"),
            "--scenario",
            "files",
        ])
        .env("EPOCH_SESSION_ID", "trusted-session")
        .env("EPOCH_BRANCH_ID", "trusted-branch")
        .output()
        .expect("test agent binary should launch");

    assert!(output.status.success());
    let first = output
        .stdout
        .split(|byte| *byte == b'\n')
        .next()
        .expect("first record");
    let start = decode_line(first).expect("valid start record");
    let Message::AgentStart(start) = start.message else {
        panic!("first record was not agent.start");
    };
    assert_eq!(start.session_id, "trusted-session");
    assert_eq!(start.branch_id, "trusted-branch");
}

#[test]
fn cli_rejects_an_incomplete_supervisor_binding() {
    let workspace = TestDir::new();
    let output = Command::new(env!("CARGO_BIN_EXE_epoch-test-agent"))
        .args([
            "--workspace",
            workspace.path().to_str().expect("UTF-8 test path"),
            "--scenario",
            "files",
        ])
        .env("EPOCH_SESSION_ID", "trusted-session")
        .env_remove("EPOCH_BRANCH_ID")
        .output()
        .expect("test agent binary should launch");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be set together"));
}
