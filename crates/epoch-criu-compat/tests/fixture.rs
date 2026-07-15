use std::{
    fs,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

#[test]
fn fixture_becomes_ready_and_exhibits_restorable_progress() {
    let workspace = TempDir::new().expect("workspace");
    let mut fixture = Command::new(env!("CARGO_BIN_EXE_epoch-criu-fixture"))
        .args([
            "--scenario",
            "sleeping_process",
            "--workspace",
            workspace.path().to_str().expect("UTF-8 path"),
            "--memory-bytes",
            "1048576",
            "--process-count",
            "1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch fixture");
    wait_for_file(&workspace.path().join("ready"), Duration::from_secs(2));
    let first = read_counter(&workspace.path().join("heartbeat"));
    thread::sleep(Duration::from_millis(150));
    let second = read_counter(&workspace.path().join("heartbeat"));
    fixture.kill().expect("kill fixture");
    fixture.wait().expect("reap fixture");
    assert!(second > first, "fixture did not make observable progress");
}

fn wait_for_file(path: &std::path::Path, timeout: Duration) {
    let started = Instant::now();
    while !path.is_file() {
        assert!(
            started.elapsed() < timeout,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn read_counter(path: &std::path::Path) -> u64 {
    fs::read_to_string(path)
        .expect("counter")
        .trim()
        .parse()
        .expect("numeric counter")
}
