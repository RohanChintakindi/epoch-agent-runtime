use std::{collections::BTreeMap, fs, path::Path, process::Stdio};

use epoch_sandbox::{
    BackendKind, BackendOutcome, BackendStatus, DirectBackend, ExecutionBackend, LaunchRequest,
    LinuxBackend, LinuxTools, ResourceLimits, UnsupportedCode, plan_linux_launch,
};
use tempfile::TempDir;

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::write(path, b"fixture").expect("write executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("permissions");
}

#[cfg(not(unix))]
fn make_executable(path: &Path) {
    fs::write(path, b"fixture").expect("write executable");
}

struct Fixture {
    _directory: TempDir,
    executable: std::path::PathBuf,
    helper: std::path::PathBuf,
    workspace: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().expect("runtime");
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace");
        let executable = directory.path().join("agent");
        let helper = directory.path().join("epoch-sandbox-init");
        make_executable(&executable);
        make_executable(&helper);
        Self {
            _directory: directory,
            executable,
            helper,
            workspace,
        }
    }

    fn request(&self) -> LaunchRequest {
        LaunchRequest::new(
            &self.executable,
            ["--scenario", "full"],
            &self.workspace,
            &self.workspace,
            BTreeMap::from([
                ("EPOCH_BRANCH_ID".to_owned(), "branch-1".to_owned()),
                ("EPOCH_SESSION_ID".to_owned(), "session-1".to_owned()),
            ]),
            &self.helper,
            ResourceLimits::new(64 * 1024 * 1024, 16, 50).expect("limits"),
        )
        .expect("request")
    }
}

fn tools() -> LinuxTools {
    LinuxTools::new("/usr/bin/systemd-run", "/usr/bin/bwrap", "/usr/bin/systemctl")
        .expect("tools")
}

#[test]
fn direct_backend_is_preserved_behind_the_common_interface() {
    let fixture = Fixture::new();
    let backend = DirectBackend;
    assert_eq!(backend.kind(), BackendKind::Direct);
    assert_eq!(backend.capabilities().status, BackendStatus::Supported);
    let BackendOutcome::Supported(plan) = backend.prepare(&fixture.request()) else {
        panic!("direct backend must be available");
    };
    assert_eq!(plan.backend(), BackendKind::Direct);
    assert_eq!(plan.program(), fixture.executable);
    assert_eq!(plan.arguments(), ["--scenario", "full"]);
    assert!(plan.clear_environment());
}

#[test]
fn linux_plan_has_every_required_boundary_and_no_shell_interpolation() {
    let fixture = Fixture::new();
    let request = fixture.request();
    let plan = plan_linux_launch(&request, &tools(), "epoch-test-123").expect("plan");
    assert_eq!(plan.backend(), BackendKind::Linux);
    assert_eq!(plan.program(), Path::new("/usr/bin/systemd-run"));
    let arguments = plan.arguments();

    for required in [
        "--scope",
        "--collect",
        "MemoryMax=67108864",
        "TasksMax=16",
        "CPUQuota=50%",
        "/usr/bin/bwrap",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-cgroup-try",
        "--ro-bind",
        "--bind",
        "--tmpfs",
        "--proc",
        "--dev",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--die-with-parent",
        "--new-session",
        "--seccomp-profile-v1",
    ] {
        assert!(arguments.iter().any(|argument| argument == required), "missing {required}");
    }
    assert!(!arguments.iter().any(|argument| {
        matches!(argument.as_str(), "sh" | "/bin/sh" | "bash" | "/bin/bash" | "-c")
    }));
    assert!(arguments.ends_with(&[
        fixture.executable.display().to_string(),
        "--scenario".to_owned(),
        "full".to_owned(),
    ]));
}

#[test]
fn request_validation_rejects_symlink_escapes_and_unsafe_environment() {
    let fixture = Fixture::new();
    let outside = fixture._directory.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, fixture.workspace.join("escape")).expect("symlink");

    let bad_working_directory = LaunchRequest::new(
        &fixture.executable,
        std::iter::empty::<&str>(),
        &fixture.workspace,
        fixture.workspace.join("escape"),
        BTreeMap::new(),
        &fixture.helper,
        ResourceLimits::new(1_048_576, 2, 25).expect("limits"),
    );
    assert!(bad_working_directory.is_err());

    for name in ["OPENAI_API_KEY", "LD_PRELOAD", "bad=name", "PATH\nINJECT"] {
        let result = LaunchRequest::new(
            &fixture.executable,
            std::iter::empty::<&str>(),
            &fixture.workspace,
            &fixture.workspace,
            BTreeMap::from([(name.to_owned(), "secret".to_owned())]),
            &fixture.helper,
            ResourceLimits::new(1_048_576, 2, 25).expect("limits"),
        );
        assert!(result.is_err(), "accepted unsafe environment key {name:?}");
    }
}

#[test]
fn resource_limits_are_bounded_before_command_construction() {
    assert!(ResourceLimits::new(0, 4, 100).is_err());
    assert!(ResourceLimits::new(64 * 1024 * 1024, 0, 100).is_err());
    assert!(ResourceLimits::new(64 * 1024 * 1024, 4, 0).is_err());
    assert!(ResourceLimits::new(u64::MAX, 4, 100).is_err());
    assert!(ResourceLimits::new(64 * 1024 * 1024, u32::MAX, 100).is_err());
    assert!(ResourceLimits::new(64 * 1024 * 1024, 4, 1001).is_err());
}

#[test]
fn linux_backend_never_silently_falls_back_to_direct() {
    let fixture = Fixture::new();
    let backend = LinuxBackend::discover();
    if backend.capabilities().status == BackendStatus::Unsupported {
        let BackendOutcome::Unsupported(reason) = backend.prepare(&fixture.request()) else {
            panic!("unsupported Linux backend must stay unsupported");
        };
        assert_ne!(reason.code, UnsupportedCode::DirectFallback);
        assert!(!reason.detail.is_empty());
    }
}

#[test]
fn direct_backend_can_launch_without_a_shell() {
    let executable = if cfg!(windows) { "cmd.exe" } else { "/bin/true" };
    let directory = TempDir::new().expect("runtime");
    let helper = directory.path().join("helper");
    make_executable(&helper);
    let request = LaunchRequest::new(
        executable,
        std::iter::empty::<&str>(),
        directory.path(),
        directory.path(),
        BTreeMap::new(),
        helper,
        ResourceLimits::new(1_048_576, 2, 25).expect("limits"),
    )
    .expect("request");
    let BackendOutcome::Supported(plan) = DirectBackend.prepare(&request) else {
        panic!("direct plan");
    };
    let mut process = DirectBackend
        .launch(plan, Stdio::null(), Stdio::null())
        .expect("launch");
    assert!(DirectBackend.cleanup(&mut process).is_ok());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn non_linux_hosts_report_structured_unsupported() {
    let backend = LinuxBackend::discover();
    let capabilities = backend.capabilities();
    assert_eq!(capabilities.status, BackendStatus::Unsupported);
    assert!(capabilities.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == UnsupportedCode::PlatformNotLinux
    }));
}
