#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use epoch_core::{EventKind, EventStatus};
use epoch_events::{EventJournal, EventQuery};
use epoch_protocol::{
    AgentStart, Completion, CompletionOutcome, Envelope, Extensions, Message, encode_line,
};
use epoch_storage::Store;
use epoch_supervisor::{
    AgentTermination, DirectSupervisor, ExecutionError, MAX_STDERR_BYTES, SupervisorError,
};
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().expect("create supervisor fixture");
        let state = directory.path().join("state");
        Self { directory, state }
    }

    fn script(&self, body: &str) -> PathBuf {
        let path = self.directory.path().join("agent.sh");
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}")).expect("write agent script");
        let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("make agent executable");
        path
    }

    fn manifest(&self, executable: &Path) -> PathBuf {
        let path = self.directory.path().join("workload.toml");
        let executable = executable.display();
        fs::write(
            &path,
            format!(
                "schema_version = 1\nname = \"fixture-agent\"\nexecutable = \"{executable}\"\n"
            ),
        )
        .expect("write workload manifest");
        path
    }

    fn supervisor(&self) -> DirectSupervisor {
        DirectSupervisor::open(&self.state).expect("open direct supervisor")
    }
}

fn record(sequence: u64, message: Message) -> String {
    encode_line(&Envelope::new(sequence, message)).expect("encode fixture record")
}

fn start_record() -> String {
    record(
        0,
        Message::AgentStart(AgentStart {
            agent_id: "fixture-agent".to_owned(),
            session_id: "__EPOCH_SESSION_ID__".to_owned(),
            branch_id: "__EPOCH_BRANCH_ID__".to_owned(),
            extensions: Extensions::new(),
        }),
    )
}

fn completion_record() -> String {
    record(
        1,
        Message::Completion(Completion {
            outcome: CompletionOutcome::Succeeded,
            output_hash: None,
            extensions: Extensions::new(),
        }),
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn emit_stdout(value: &str) -> String {
    format!("printf '%s' {}\n", shell_quote(value))
}

fn emit_start() -> String {
    let record = start_record();
    let session_position = record
        .find("__EPOCH_SESSION_ID__")
        .expect("session placeholder");
    let branch_position = record
        .find("__EPOCH_BRANCH_ID__")
        .expect("branch placeholder");
    let template = record
        .replace("__EPOCH_SESSION_ID__", "%s")
        .replace("__EPOCH_BRANCH_ID__", "%s");
    let arguments = if session_position < branch_position {
        "\"$EPOCH_SESSION_ID\" \"$EPOCH_BRANCH_ID\""
    } else {
        "\"$EPOCH_BRANCH_ID\" \"$EPOCH_SESSION_ID\""
    };
    format!("printf {} {arguments}\n", shell_quote(&template))
}

fn successful_body(stderr: &str) -> String {
    format!(
        "{}{}printf '%s' {} >&2\nexit 0\n",
        emit_start(),
        emit_stdout(&completion_record()),
        shell_quote(stderr)
    )
}

fn state(store: &Store, session_id: &str, branch_id: &str) -> (String, String) {
    let session = store
        .connection()
        .query_row(
            "SELECT state FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .expect("read session state");
    let branch = store
        .connection()
        .query_row(
            "SELECT state FROM branches WHERE id = ?1",
            [branch_id],
            |row| row.get(0),
        )
        .expect("read branch state");
    (session, branch)
}

#[test]
fn successful_run_persists_lifecycle_boundary_events_and_stderr() {
    let fixture = Fixture::new();
    let script = fixture.script(&successful_body("diagnostic summary\n"));
    let manifest = fixture.manifest(&script);

    let outcome = fixture
        .supervisor()
        .run_manifest(manifest)
        .expect("run successful workload");

    assert_eq!(outcome.termination, AgentTermination::Succeeded { code: 0 });
    assert_eq!(outcome.protocol_records, 2);
    assert_eq!(outcome.stderr, b"diagnostic summary\n");
    let store = Store::open(fixture.state.join("state.db")).expect("reopen state database");
    assert_eq!(
        state(
            &store,
            &outcome.session_id.to_string(),
            &outcome.branch_id.to_string()
        ),
        ("completed".to_owned(), "completed".to_owned())
    );
    drop(store);

    let journal = EventJournal::open(fixture.state.join("state.db"), fixture.state.join("blobs"))
        .expect("reopen event journal");
    let events = journal
        .query(&EventQuery::for_session(outcome.session_id))
        .expect("query committed history");
    let kinds = events
        .iter()
        .map(|event| event.kind.as_str())
        .collect::<Vec<_>>();
    for expected in [
        "supervisor.run_started",
        "process.started",
        "agent.start",
        "agent.completion",
        "process.stderr",
        "process.exited",
    ] {
        assert!(kinds.contains(&expected), "missing {expected}: {kinds:?}");
    }
    let stderr_event = events
        .iter()
        .find(|event| event.kind.as_str() == "process.stderr")
        .expect("stderr event");
    let stderr_payload = journal
        .read_payload(stderr_event)
        .expect("read durable stderr payload");
    assert_eq!(
        stderr_payload["bytes"],
        serde_json::json!(b"diagnostic summary\n")
    );
}

#[test]
fn nonzero_agent_exit_is_a_successful_supervision_result_with_failed_lifecycle() {
    let fixture = Fixture::new();
    let body = format!(
        "{}printf '%s' 'injected crash' >&2\nexit 70\n",
        emit_start()
    );
    let script = fixture.script(&body);
    let outcome = fixture
        .supervisor()
        .run_manifest(fixture.manifest(&script))
        .expect("nonzero agent exit is not a supervisor failure");

    assert_eq!(
        outcome.termination,
        AgentTermination::NonZero {
            code: Some(70),
            signal: None
        }
    );
    let store = Store::open(fixture.state.join("state.db")).expect("open state database");
    assert_eq!(
        state(
            &store,
            &outcome.session_id.to_string(),
            &outcome.branch_id.to_string()
        ),
        ("failed".to_owned(), "failed".to_owned())
    );
}

#[test]
fn malformed_stdout_is_a_supervisor_failure_and_cleans_up_the_process_group() {
    let fixture = Fixture::new();
    let child_pid = fixture.directory.path().join("descendant.pid");
    let body = format!(
        "/bin/sleep 30 &\necho $! > {}\n{}{}\n/bin/sleep 30\n",
        shell_quote(&child_pid.display().to_string()),
        emit_start(),
        emit_stdout("{not-json}\n")
    );
    let script = fixture.script(&body);
    let error = fixture
        .supervisor()
        .run_manifest(fixture.manifest(&script))
        .expect_err("malformed boundary output must fail supervision");
    assert!(matches!(
        error,
        SupervisorError::Execution {
            source: ExecutionError::Protocol { line: 2, .. },
            ..
        }
    ));

    let pid = fs::read_to_string(child_pid)
        .expect("descendant pid was recorded")
        .trim()
        .to_owned();
    for _ in 0..20 {
        if !Command::new("/bin/kill")
            .args(["-0", &pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("descendant process {pid} survived supervisor failure");
}

#[test]
fn partial_final_stdout_record_is_reported_explicitly() {
    let fixture = Fixture::new();
    let script = fixture.script(&emit_stdout("{\"protocol_version\":1"));
    let error = fixture
        .supervisor()
        .run_manifest(fixture.manifest(&script))
        .expect_err("partial record must fail supervision");
    assert!(
        matches!(
            error,
            SupervisorError::Execution {
                source: ExecutionError::PartialFinalRecord { line: 1, .. },
                ..
            }
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
fn committed_history_is_queryable_after_supervisor_restart() {
    let fixture = Fixture::new();
    let script = fixture.script(&successful_body(""));
    let manifest = fixture.manifest(&script);
    let outcome = fixture
        .supervisor()
        .run_manifest(&manifest)
        .expect("run workload");

    let restarted = DirectSupervisor::open(&fixture.state).expect("restart supervisor");
    drop(restarted);
    let journal = EventJournal::open(fixture.state.join("state.db"), fixture.state.join("blobs"))
        .expect("reopen journal");
    let events = journal
        .query(&EventQuery::for_session(outcome.session_id))
        .expect("query history after restart");
    assert!(events.len() >= 5);
    assert_eq!(
        events.last().map(|event| event.kind.clone()),
        Some(EventKind::new("process.exited").expect("valid kind"))
    );
}

#[test]
fn large_stderr_is_drained_concurrently_and_bounded() {
    let fixture = Fixture::new();
    let stderr = "x".repeat(MAX_STDERR_BYTES / 2);
    let script = fixture.script(&successful_body(&stderr));
    let outcome = fixture
        .supervisor()
        .run_manifest(fixture.manifest(&script))
        .expect("large stderr must not deadlock");
    assert_eq!(outcome.stderr.len(), stderr.len());

    let oversized = "x".repeat(MAX_STDERR_BYTES + 1);
    let script = fixture.script(&successful_body(&oversized));
    let error = fixture
        .supervisor()
        .run_manifest(fixture.manifest(&script))
        .expect_err("oversized stderr must fail closed");
    assert!(
        matches!(
            error,
            SupervisorError::Execution {
                source: ExecutionError::StderrTooLarge {
                    maximum: MAX_STDERR_BYTES
                },
                ..
            }
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
fn supervisor_failure_history_preserves_the_valid_protocol_prefix() {
    let fixture = Fixture::new();
    let body = format!("{}{}", emit_start(), emit_stdout("bad\n"));
    let script = fixture.script(&body);
    let error = fixture
        .supervisor()
        .run_manifest(fixture.manifest(&script))
        .expect_err("malformed output must fail");
    let session_id = error
        .session_id()
        .expect("execution failure has session id");

    let journal = EventJournal::open(fixture.state.join("state.db"), fixture.state.join("blobs"))
        .expect("reopen journal");
    let events = journal
        .query(&EventQuery::for_session(session_id))
        .expect("query failure history");
    assert!(
        events
            .iter()
            .any(|event| event.kind.as_str() == "agent.start")
    );
    assert!(events.iter().any(|event| {
        event.kind.as_str() == "supervisor.failure" && event.status == EventStatus::Failed
    }));
}
