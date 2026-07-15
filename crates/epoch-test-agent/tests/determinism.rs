use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use epoch_protocol::{Message, decode_line};
use epoch_test_agent::{Scenario, WorkloadConfig, run_workload};

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let suffix = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "epoch-agent-{label}-{}-{suffix}",
            std::process::id()
        ));
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

fn run(seed: u64, scenario: Scenario, workspace: &Path) -> (Vec<u8>, epoch_test_agent::RunSummary) {
    let config = WorkloadConfig::new(seed, scenario, workspace.to_path_buf());
    let mut trace = Vec::new();
    let summary = run_workload(&config, &mut trace).expect("workload should succeed");
    (trace, summary)
}

fn messages(trace: &[u8]) -> Vec<Message> {
    std::str::from_utf8(trace)
        .expect("trace should be UTF-8 JSONL")
        .lines()
        .map(|line| {
            decode_line(line.as_bytes())
                .expect("agent must emit valid protocol records")
                .message
        })
        .collect()
}

#[test]
fn same_seed_produces_identical_trace_and_normalized_state() {
    let first = TestDir::new("same-seed-a");
    let second = TestDir::new("same-seed-b");

    let (first_trace, first_summary) = run(0x5eed, Scenario::Full, first.path());
    let (second_trace, second_summary) = run(0x5eed, Scenario::Full, second.path());

    assert_eq!(first_trace, second_trace);
    assert_eq!(first_summary, second_summary);
    assert_eq!(first_summary.state_hash, second_summary.state_hash);
    assert_eq!(
        first_summary.normalized_trace_hash,
        second_summary.normalized_trace_hash
    );
}

#[test]
fn seed_changes_both_trace_and_state_hash() {
    let first = TestDir::new("different-seed-a");
    let second = TestDir::new("different-seed-b");

    let (first_trace, first_summary) = run(11, Scenario::Full, first.path());
    let (second_trace, second_summary) = run(12, Scenario::Full, second.path());

    assert_ne!(first_trace, second_trace);
    assert_ne!(first_summary.state_hash, second_summary.state_hash);
    assert_ne!(
        first_summary.normalized_trace_hash,
        second_summary.normalized_trace_hash
    );
}

#[test]
fn full_scenario_exercises_state_process_workspace_and_loopback_network() {
    let workspace = TestDir::new("full");
    let (trace, summary) = run(42, Scenario::Full, workspace.path());
    let messages = messages(&trace);
    let kinds: Vec<_> = messages.iter().map(Message::kind).collect();
    let tools: Vec<_> = messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolCall(call) => Some(call.tool.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(kinds.first(), Some(&"agent.start"));
    assert!(kinds.contains(&"context.update"));
    assert!(kinds.contains(&"model.request"));
    assert!(kinds.contains(&"model.response"));
    assert!(kinds.contains(&"safe_point"));
    assert_eq!(kinds.last(), Some(&"agent.completion"));
    assert_eq!(
        tools,
        [
            "file.create",
            "file.append",
            "memory.allocate",
            "process.spawn",
            "network.loopback",
        ]
    );
    assert_eq!(
        summary.event_count,
        u64::try_from(messages.len()).expect("test trace length should fit in u64")
    );

    let artifact = fs::read_to_string(workspace.path().join("artifact.txt"))
        .expect("file scenario should create its artifact");
    assert!(artifact.contains("seed=42"));
    assert!(artifact.contains("mutation="));
    assert_eq!(summary.state.files.len(), 1);
    assert_eq!(
        summary.state.memory.as_ref().map(|state| state.bytes),
        Some(64 * 1024)
    );
    assert_eq!(
        summary.state.child.as_ref().map(|state| state.exit_code),
        Some(0)
    );
    assert!(summary.state.network.is_some());
}

#[test]
fn individual_scenarios_execute_only_the_selected_tools() {
    let cases = [
        (Scenario::Files, vec!["file.create", "file.append"]),
        (Scenario::Memory, vec!["memory.allocate"]),
        (Scenario::Child, vec!["process.spawn"]),
        (Scenario::Network, vec!["network.loopback"]),
    ];

    for (scenario, expected) in cases {
        let workspace = TestDir::new(scenario.as_str());
        let (trace, _) = run(7, scenario, workspace.path());
        let tools: Vec<_> = messages(&trace)
            .into_iter()
            .filter_map(|message| match message {
                Message::ToolCall(call) => Some(call.tool),
                _ => None,
            })
            .collect();
        assert_eq!(tools, expected, "wrong tools for {scenario:?}");
    }
}
