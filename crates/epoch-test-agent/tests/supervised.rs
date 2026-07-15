#![cfg(unix)]

use std::fs;

use epoch_supervisor::{AgentTermination, DirectSupervisor, EventPageRequest};
use tempfile::TempDir;

#[test]
fn real_agent_stream_runs_under_the_supervisors_execution_binding() {
    let fixture = TempDir::new().expect("create supervised agent fixture");
    let workspace = fixture.path().join("workspace");
    let manifest = fixture.path().join("workload.toml");
    let executable = env!("CARGO_BIN_EXE_epoch-test-agent");
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\n\
             name = \"epoch-test-agent\"\n\
             executable = \"{executable}\"\n\
             arguments = [\"--scenario\", \"files\", \"--workspace\", \"{}\"]\n",
            workspace.display()
        ),
    )
    .expect("write supervised workload manifest");

    let state_root = fixture.path().join("state");
    let supervisor = DirectSupervisor::open(&state_root).expect("open supervisor");
    let outcome = supervisor
        .run_manifest(manifest)
        .expect("supervise the real deterministic agent");

    assert_eq!(outcome.termination, AgentTermination::Succeeded { code: 0 });
    assert_eq!(outcome.protocol_records, 11);
    assert!(
        !outcome.stderr.is_empty(),
        "agent summary should be captured"
    );
    assert!(workspace.join("artifact.txt").is_file());

    drop(supervisor);
    let restarted = DirectSupervisor::open_existing(&state_root).expect("restart supervisor");
    let status = restarted
        .session_status(outcome.session_id)
        .expect("inspect durable session status");
    assert_eq!(status.state, "completed");
    assert_eq!(status.branches.len(), 1);
    assert_eq!(status.branches[0].branch_id, outcome.branch_id);

    let page = restarted
        .events(EventPageRequest {
            session_id: outcome.session_id,
            branch_id: Some(outcome.branch_id),
            offset: 0,
            limit: 100,
        })
        .expect("inspect durable event timeline");
    assert!(!page.has_more);
    assert!(page.events.len() > 10);
    assert!(
        page.events
            .iter()
            .any(|event| event.kind == "process.manifest")
    );
}
