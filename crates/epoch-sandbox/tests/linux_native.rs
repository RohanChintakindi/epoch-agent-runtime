#![cfg(target_os = "linux")]

use std::{collections::BTreeMap, fs, io::Read as _, process::Stdio, time::Instant};

use epoch_sandbox::{
    BackendOutcome, BackendStatus, DirectBackend, ExecutionBackend, LaunchRequest, LinuxBackend,
    ResourceLimits,
};
use tempfile::TempDir;

fn enabled() -> bool {
    std::env::var("EPOCH_RUN_PRIVILEGED_ISOLATION").as_deref() == Ok("1")
}

struct Fixture {
    directory: TempDir,
    helper: &'static str,
    probe: &'static str,
}

impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = TempDir::new().expect("runtime");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o777))
            .expect("sandbox user workspace permissions");
        Self {
            directory,
            helper: env!("CARGO_BIN_EXE_epoch-sandbox-init"),
            probe: env!("CARGO_BIN_EXE_epoch-sandbox-probe"),
        }
    }

    fn request(&self, mode: &str, limits: ResourceLimits) -> LaunchRequest {
        LaunchRequest::new(
            self.probe,
            [mode],
            self.directory.path(),
            self.directory.path(),
            BTreeMap::new(),
            self.helper,
            limits,
        )
        .expect("request")
    }
}

fn launch_and_collect(
    backend: &LinuxBackend,
    request: &LaunchRequest,
) -> (std::process::ExitStatus, String, String, String) {
    let BackendOutcome::Supported(plan) = backend.prepare(request) else {
        panic!("privileged Oracle host must prepare Linux sandbox");
    };
    let unit = plan
        .arguments()
        .windows(2)
        .find(|pair| pair[0] == "--unit")
        .map(|pair| pair[1].clone())
        .expect("unit");
    let mut process = backend
        .launch(plan, Stdio::piped(), Stdio::piped())
        .expect("launch");
    let mut stdout = String::new();
    process
        .child_mut()
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    process
        .child_mut()
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    let status = process.child_mut().wait().expect("wait");
    backend.cleanup(&mut process).expect("cleanup");
    (status, stdout, stderr, unit)
}

#[test]
fn privileged_oracle_boundary_enforces_filesystem_network_pid_and_kernel_policy() {
    if !enabled() {
        return;
    }
    let fixture = Fixture::new();
    let backend = LinuxBackend::discover();
    assert_eq!(backend.capabilities().status, BackendStatus::Supported);
    let host_tmp_marker = std::path::Path::new("/tmp/epoch-private-marker");
    fs::remove_file(host_tmp_marker).ok();

    let (status, stdout, stderr, unit) = launch_and_collect(
        &backend,
        &fixture.request(
            "inspect",
            ResourceLimits::new(64 * 1024 * 1024, 16, 50).expect("limits"),
        ),
    );
    assert!(status.success(), "sandbox failed: {stderr}");
    for expected in [
        "base_read_only=true",
        "network_blocked=true",
        "child_spawned=true",
        "unshare_blocked=true",
        "status_CapPrm=0000000000000000",
        "status_CapEff=0000000000000000",
        "status_CapBnd=0000000000000000",
        "status_CapAmb=0000000000000000",
        "status_NoNewPrivs=1",
        "status_Seccomp=2",
    ] {
        assert!(stdout.contains(expected), "missing {expected} in {stdout}");
    }
    let pid = value(&stdout, "pid").parse::<u32>().expect("pid");
    let visible = value(&stdout, "numeric_processes")
        .parse::<usize>()
        .expect("process count");
    assert!(pid <= 8, "workload PID is not namespaced: {pid}");
    assert!(
        visible <= 8,
        "host processes leaked into PID namespace: {visible}"
    );
    assert_eq!(
        fs::read(fixture.directory.path().join("workspace-write.txt")).unwrap(),
        b"sandboxed\n"
    );
    assert!(
        !host_tmp_marker.exists(),
        "private tmp mount leaked to host"
    );
    assert_unit_collected(&unit);
}

#[test]
fn cgroup_pid_and_memory_limits_are_visible_and_units_are_collected() {
    if !enabled() {
        return;
    }
    let fixture = Fixture::new();
    let backend = LinuxBackend::discover();
    let (status, stdout, stderr, pid_unit) = launch_and_collect(
        &backend,
        &fixture.request(
            "fork-limit",
            ResourceLimits::new(64 * 1024 * 1024, 12, 50).expect("limits"),
        ),
    );
    assert!(status.success(), "PID probe failed: {stderr}");
    let spawned = value(&stdout, "spawned").parse::<usize>().expect("spawned");
    assert!(
        (1..12).contains(&spawned),
        "pids.max was not enforced: {spawned}"
    );
    assert_unit_collected(&pid_unit);

    let (status, _stdout, _stderr, memory_unit) = launch_and_collect(
        &backend,
        &fixture.request(
            "memory-limit",
            ResourceLimits::new(64 * 1024 * 1024, 8, 50).expect("limits"),
        ),
    );
    assert!(
        !status.success(),
        "memory.max did not terminate the allocator"
    );
    assert_unit_collected(&memory_unit);
}

#[test]
fn launch_overhead_samples_are_emitted_for_direct_and_linux_backends() {
    if !enabled() {
        return;
    }
    let fixture = Fixture::new();
    let linux = LinuxBackend::discover();
    let request = fixture.request(
        "inspect",
        ResourceLimits::new(64 * 1024 * 1024, 16, 100).expect("limits"),
    );
    let mut direct_samples = Vec::new();
    let mut linux_samples = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let BackendOutcome::Supported(plan) = DirectBackend.prepare(&request) else {
            panic!("direct");
        };
        let mut process = DirectBackend
            .launch(plan, Stdio::null(), Stdio::null())
            .expect("direct launch");
        DirectBackend.cleanup(&mut process).expect("direct cleanup");
        direct_samples.push(started.elapsed().as_nanos());

        let started = Instant::now();
        let (status, _, stderr, _) = launch_and_collect(&linux, &request);
        assert!(status.success(), "Linux sample failed: {stderr}");
        linux_samples.push(started.elapsed().as_nanos());
    }
    println!("direct_launch_ns={direct_samples:?}");
    println!("linux_launch_ns={linux_samples:?}");
    assert_eq!(direct_samples.len(), linux_samples.len());
}

fn value<'a>(output: &'a str, key: &str) -> &'a str {
    output
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("missing {key} in {output}"))
}

fn assert_unit_collected(unit: &str) {
    let output = std::process::Command::new("/usr/bin/systemctl")
        .args(["show", "--property", "LoadState", "--value", unit])
        .env_clear()
        .output()
        .expect("systemctl show");
    let state = String::from_utf8_lossy(&output.stdout);
    assert!(
        state.trim().is_empty() || state.trim() == "not-found",
        "unit remains: {unit} {state}"
    );
}
