#![cfg(unix)]

use std::fs;

use epoch_supervisor::{AgentTermination, DirectSupervisor};
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

    let outcome = DirectSupervisor::open(fixture.path().join("state"))
        .expect("open supervisor")
        .run_manifest(manifest)
        .expect("supervise the real deterministic agent");

    assert_eq!(outcome.termination, AgentTermination::Succeeded { code: 0 });
    assert_eq!(outcome.protocol_records, 11);
    assert!(
        !outcome.stderr.is_empty(),
        "agent summary should be captured"
    );
    assert!(workspace.join("artifact.txt").is_file());
}
