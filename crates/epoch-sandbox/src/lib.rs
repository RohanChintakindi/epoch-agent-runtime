//! Fail-closed execution-backend contracts and Linux isolation.
//!
//! The direct backend remains a baseline. The Linux backend is a separately discovered boundary:
//! if any required facility is absent, preparation returns [`BackendOutcome::Unsupported`] and
//! never constructs or launches a direct process instead.

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
    process::{Child, ExitStatus, Stdio},
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const MIN_MEMORY_BYTES: u64 = 1024 * 1024;
const MAX_MEMORY_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
const MAX_PIDS: u32 = 4096;
const MAX_CPU_PERCENT: u16 = 1000;
const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_ENVIRONMENT_ENTRIES: usize = 64;
const MAX_ENVIRONMENT_VALUE_BYTES: usize = 4096;

/// Stable execution backend identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Direct,
    Linux,
}

/// Whether a backend is actually registered and usable on the current host.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendStatus {
    Supported,
    Unsupported,
}

/// Stable reason a required Linux boundary is unavailable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedCode {
    PlatformNotLinux,
    PrivilegeRequired,
    MissingTool,
    MissingCgroupV2,
    MissingCgroupController,
    UnsupportedArchitecture,
    KernelProbeFailed,
    SeccompUnavailable,
    DirectFallback,
}

/// One structured capability-discovery result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityDiagnostic {
    pub code: UnsupportedCode,
    pub facility: String,
    pub detail: String,
}

/// Explicit current-host backend capability report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackendCapabilities {
    pub backend: BackendKind,
    pub status: BackendStatus,
    pub diagnostics: Vec<CapabilityDiagnostic>,
}

/// Structured unsupported result, distinct from launch or workload failure.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UnsupportedBackend {
    pub backend: BackendKind,
    pub code: UnsupportedCode,
    pub detail: String,
}

/// A backend operation either has a real implementation or a structured incompatibility.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendOutcome<T> {
    Supported(T),
    Unsupported(UnsupportedBackend),
}

/// Cgroup-v2 resource controls applied before the workload executes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ResourceLimits {
    memory_bytes: u64,
    pids: u32,
    cpu_percent: u16,
}

impl ResourceLimits {
    /// Creates bounded CPU, memory, and PID limits.
    ///
    /// # Errors
    ///
    /// Returns an error for zero, impractically small, or prototype-unbounded values.
    pub fn new(memory_bytes: u64, pids: u32, cpu_percent: u16) -> Result<Self, SandboxError> {
        if !(MIN_MEMORY_BYTES..=MAX_MEMORY_BYTES).contains(&memory_bytes) {
            return Err(SandboxError::InvalidLimit {
                field: "memory_bytes",
            });
        }
        if !(1..=MAX_PIDS).contains(&pids) {
            return Err(SandboxError::InvalidLimit { field: "pids" });
        }
        if !(1..=MAX_CPU_PERCENT).contains(&cpu_percent) {
            return Err(SandboxError::InvalidLimit {
                field: "cpu_percent",
            });
        }
        Ok(Self {
            memory_bytes,
            pids,
            cpu_percent,
        })
    }

    #[must_use]
    pub const fn memory_bytes(self) -> u64 {
        self.memory_bytes
    }

    #[must_use]
    pub const fn pids(self) -> u32 {
        self.pids
    }

    #[must_use]
    pub const fn cpu_percent(self) -> u16 {
        self.cpu_percent
    }
}

/// Validated workload and sandbox inputs. All paths are canonical and all arguments are literal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchRequest {
    executable: PathBuf,
    arguments: Vec<String>,
    workspace: PathBuf,
    working_directory: PathBuf,
    environment: BTreeMap<String, String>,
    trusted_helper: PathBuf,
    limits: ResourceLimits,
}

impl LaunchRequest {
    /// Validates launch data before any command or cgroup is constructed.
    ///
    /// # Errors
    ///
    /// Rejects invalid executables, symlinked workspace/helper paths, work directories outside the
    /// workspace, unbounded arguments, and ambient/credential-bearing environment entries.
    #[allow(clippy::too_many_arguments)]
    pub fn new<I, S>(
        executable: impl AsRef<Path>,
        arguments: I,
        workspace: impl AsRef<Path>,
        working_directory: impl AsRef<Path>,
        environment: BTreeMap<String, String>,
        trusted_helper: impl AsRef<Path>,
        limits: ResourceLimits,
    ) -> Result<Self, SandboxError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let executable = canonical_regular_executable(executable.as_ref(), "executable", false)?;
        let trusted_helper =
            canonical_regular_executable(trusted_helper.as_ref(), "trusted_helper", true)?;
        let workspace = canonical_directory(workspace.as_ref(), "workspace", true)?;
        let working_directory =
            canonical_directory(working_directory.as_ref(), "working_directory", true)?;
        if !working_directory.starts_with(&workspace) {
            return Err(SandboxError::PathOutsideWorkspace {
                path: working_directory,
                workspace,
            });
        }

        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        if arguments.len() > MAX_ARGUMENTS {
            return Err(SandboxError::TooManyArguments {
                actual: arguments.len(),
                maximum: MAX_ARGUMENTS,
            });
        }
        for (index, argument) in arguments.iter().enumerate() {
            if argument.len() > MAX_ARGUMENT_BYTES || argument.contains('\0') {
                return Err(SandboxError::InvalidArgument { index });
            }
        }
        validate_environment(&environment)?;
        Ok(Self {
            executable,
            arguments,
            workspace,
            working_directory,
            environment,
            trusted_helper,
            limits,
        })
    }

    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    #[must_use]
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }
}

/// Absolute trusted tool locations used by the Linux launcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinuxTools {
    systemd_run: PathBuf,
    bwrap: PathBuf,
    systemctl: PathBuf,
}

impl LinuxTools {
    /// Creates a syntactically validated tool set. Host discovery separately verifies files.
    ///
    /// # Errors
    ///
    /// Returns an error unless all paths are absolute, normalized, and NUL-free.
    pub fn new(
        systemd_run: impl AsRef<Path>,
        bwrap: impl AsRef<Path>,
        systemctl: impl AsRef<Path>,
    ) -> Result<Self, SandboxError> {
        Ok(Self {
            systemd_run: validate_absolute_tool(systemd_run.as_ref())?,
            bwrap: validate_absolute_tool(bwrap.as_ref())?,
            systemctl: validate_absolute_tool(systemctl.as_ref())?,
        })
    }
}

/// Literal no-shell command plan produced by a backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedLaunch {
    backend: BackendKind,
    program: PathBuf,
    arguments: Vec<String>,
    current_directory: PathBuf,
    environment: BTreeMap<String, String>,
    clear_environment: bool,
    cgroup_unit: Option<String>,
    cleanup_program: Option<PathBuf>,
}

impl PreparedLaunch {
    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }

    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    #[must_use]
    pub const fn clear_environment(&self) -> bool {
        self.clear_environment
    }
}

/// A launched process plus cleanup identity owned by its backend.
#[derive(Debug)]
pub struct SandboxProcess {
    child: Child,
    backend: BackendKind,
    cgroup_unit: Option<String>,
    cleanup_program: Option<PathBuf>,
}

impl SandboxProcess {
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        self.backend
    }

    #[must_use]
    pub fn cgroup_unit(&self) -> Option<&str> {
        self.cgroup_unit.as_deref()
    }

    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

/// Current nonblocking process inspection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessInspection {
    pub pid: u32,
    pub exit_status: Option<ExitStatus>,
    pub cgroup_unit: Option<String>,
}

/// Common backend lifecycle used by the supervisor integration seam.
pub trait ExecutionBackend {
    fn kind(&self) -> BackendKind;
    fn capabilities(&self) -> BackendCapabilities;
    fn prepare(&self, request: &LaunchRequest) -> BackendOutcome<PreparedLaunch>;
    /// Launches an already prepared literal command.
    ///
    /// # Errors
    ///
    /// Returns an error if the command cannot be started or the backend is unsupported.
    fn launch(
        &self,
        plan: PreparedLaunch,
        stdout: Stdio,
        stderr: Stdio,
    ) -> Result<SandboxProcess, SandboxError>;
    /// Reads current process state without blocking.
    ///
    /// # Errors
    ///
    /// Returns an operating-system inspection error.
    fn inspect(&self, process: &mut SandboxProcess) -> Result<ProcessInspection, SandboxError>;
    /// Suspends the complete backend process boundary.
    ///
    /// # Errors
    ///
    /// Returns a signal or cgroup-control error.
    fn suspend(&self, process: &mut SandboxProcess) -> Result<(), SandboxError>;
    /// Resumes the complete backend process boundary.
    ///
    /// # Errors
    ///
    /// Returns a signal or cgroup-control error.
    fn resume(&self, process: &mut SandboxProcess) -> Result<(), SandboxError>;
    /// Requests termination of the complete backend process boundary.
    ///
    /// # Errors
    ///
    /// Returns a signal or cgroup-control error.
    fn terminate(&self, process: &mut SandboxProcess) -> Result<(), SandboxError>;
    /// Reaps the process and removes backend-owned resources.
    ///
    /// # Errors
    ///
    /// Returns an error if termination, waiting, or cleanup fails.
    fn cleanup(&self, process: &mut SandboxProcess) -> Result<(), SandboxError>;
}

/// Direct-process baseline behind the common backend contract.
#[derive(Clone, Copy, Debug, Default)]
pub struct DirectBackend;

impl ExecutionBackend for DirectBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Direct
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            backend: BackendKind::Direct,
            status: BackendStatus::Supported,
            diagnostics: Vec::new(),
        }
    }

    fn prepare(&self, request: &LaunchRequest) -> BackendOutcome<PreparedLaunch> {
        BackendOutcome::Supported(PreparedLaunch {
            backend: BackendKind::Direct,
            program: request.executable.clone(),
            arguments: request.arguments.clone(),
            current_directory: request.working_directory.clone(),
            environment: request.environment.clone(),
            clear_environment: true,
            cgroup_unit: None,
            cleanup_program: None,
        })
    }

    fn launch(
        &self,
        plan: PreparedLaunch,
        stdout: Stdio,
        stderr: Stdio,
    ) -> Result<SandboxProcess, SandboxError> {
        launch_plan(plan, stdout, stderr)
    }

    fn inspect(&self, process: &mut SandboxProcess) -> Result<ProcessInspection, SandboxError> {
        inspect_process(process)
    }

    fn suspend(&self, process: &mut SandboxProcess) -> Result<(), SandboxError> {
        signal_process(process, Signal::SIGSTOP)
    }

    fn resume(&self, process: &mut SandboxProcess) -> Result<(), SandboxError> {
        signal_process(process, Signal::SIGCONT)
    }

    fn terminate(&self, process: &mut SandboxProcess) -> Result<(), SandboxError> {
        terminate_process(process)
    }

    fn cleanup(&self, process: &mut SandboxProcess) -> Result<(), SandboxError> {
        cleanup_process(process)
    }
}

/// Linux namespace/cgroup/seccomp backend with explicit discovery.
#[derive(Clone, Debug)]
pub struct LinuxBackend {
    capabilities: BackendCapabilities,
    tools: Option<LinuxTools>,
}

impl LinuxBackend {
    /// Discovers every required facility without granting a direct fallback.
    #[must_use]
    pub fn discover() -> Self {
        discover_linux_backend()
    }
}

impl ExecutionBackend for LinuxBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Linux
    }

    fn capabilities(&self) -> BackendCapabilities {
        self.capabilities.clone()
    }

    fn prepare(&self, request: &LaunchRequest) -> BackendOutcome<PreparedLaunch> {
        if self.capabilities.status == BackendStatus::Unsupported {
            let diagnostic =
                self.capabilities
                    .diagnostics
                    .first()
                    .cloned()
                    .unwrap_or(CapabilityDiagnostic {
                        code: UnsupportedCode::KernelProbeFailed,
                        facility: "linux_sandbox".to_owned(),
                        detail: "Linux sandbox discovery failed without a diagnostic".to_owned(),
                    });
            return BackendOutcome::Unsupported(UnsupportedBackend {
                backend: BackendKind::Linux,
                code: diagnostic.code,
                detail: diagnostic.detail,
            });
        }
        let Some(tools) = &self.tools else {
            return BackendOutcome::Unsupported(UnsupportedBackend {
                backend: BackendKind::Linux,
                code: UnsupportedCode::KernelProbeFailed,
                detail: "supported Linux backend has no validated tool set".to_owned(),
            });
        };
        let unit = format!("epoch-{}", Uuid::new_v4().simple());
        match plan_linux_launch(request, tools, &unit) {
            Ok(plan) => BackendOutcome::Supported(plan),
            Err(error) => BackendOutcome::Unsupported(UnsupportedBackend {
                backend: BackendKind::Linux,
                code: UnsupportedCode::KernelProbeFailed,
                detail: error.to_string(),
            }),
        }
    }

    fn launch(
        &self,
        plan: PreparedLaunch,
        stdout: Stdio,
        stderr: Stdio,
    ) -> Result<SandboxProcess, SandboxError> {
        if self.capabilities.status != BackendStatus::Supported {
            return Err(SandboxError::BackendUnsupported);
        }
        launch_plan(plan, stdout, stderr)
    }

    fn inspect(&self, process: &mut SandboxProcess) -> Result<ProcessInspection, SandboxError> {
        inspect_process(process)
    }

    fn suspend(&self, process: &mut SandboxProcess) -> Result<(), SandboxError> {
        control_linux_unit(process, "SIGSTOP")
    }

    fn resume(&self, process: &mut SandboxProcess) -> Result<(), SandboxError> {
        control_linux_unit(process, "SIGCONT")
    }

    fn terminate(&self, process: &mut SandboxProcess) -> Result<(), SandboxError> {
        stop_linux_unit(process)
    }

    fn cleanup(&self, process: &mut SandboxProcess) -> Result<(), SandboxError> {
        if process.child.try_wait()?.is_none() {
            stop_linux_unit(process)?;
        }
        process.child.wait()?;
        Ok(())
    }
}

/// Constructs the literal privileged Linux launch plan after discovery.
///
/// # Errors
///
/// Returns an error for an invalid transient-unit name or inconsistent validated paths.
pub fn plan_linux_launch(
    request: &LaunchRequest,
    tools: &LinuxTools,
    unit_name: &str,
) -> Result<PreparedLaunch, SandboxError> {
    validate_unit_name(unit_name)?;
    let unit = format!("{unit_name}.scope");
    let mut arguments = vec![
        "--scope".to_owned(),
        "--quiet".to_owned(),
        "--collect".to_owned(),
        "--unit".to_owned(),
        unit.clone(),
        "--property".to_owned(),
        format!("MemoryMax={}", request.limits.memory_bytes),
        "--property".to_owned(),
        format!("TasksMax={}", request.limits.pids),
        "--property".to_owned(),
        format!("CPUQuota={}%", request.limits.cpu_percent),
        "--".to_owned(),
        tools.bwrap.display().to_string(),
        "--die-with-parent".to_owned(),
        "--new-session".to_owned(),
        "--unshare-user".to_owned(),
        "--uid".to_owned(),
        "65534".to_owned(),
        "--gid".to_owned(),
        "65534".to_owned(),
        "--unshare-pid".to_owned(),
        "--unshare-net".to_owned(),
        "--unshare-ipc".to_owned(),
        "--unshare-uts".to_owned(),
        "--unshare-cgroup-try".to_owned(),
        "--cap-drop".to_owned(),
        "ALL".to_owned(),
        "--ro-bind".to_owned(),
        "/".to_owned(),
        "/".to_owned(),
        "--bind".to_owned(),
        request.workspace.display().to_string(),
        request.workspace.display().to_string(),
        "--tmpfs".to_owned(),
        "/tmp".to_owned(),
        "--proc".to_owned(),
        "/proc".to_owned(),
        "--dev".to_owned(),
        "/dev".to_owned(),
        "--chdir".to_owned(),
        request.working_directory.display().to_string(),
        "--clearenv".to_owned(),
        "--setenv".to_owned(),
        "HOME".to_owned(),
        request.workspace.display().to_string(),
        "--setenv".to_owned(),
        "TMPDIR".to_owned(),
        "/tmp".to_owned(),
    ];
    for (key, value) in &request.environment {
        arguments.extend(["--setenv".to_owned(), key.clone(), value.clone()]);
    }
    arguments.extend([
        "--".to_owned(),
        request.trusted_helper.display().to_string(),
        "--seccomp-profile-v1".to_owned(),
        "--".to_owned(),
        request.executable.display().to_string(),
    ]);
    arguments.extend(request.arguments.iter().cloned());

    Ok(PreparedLaunch {
        backend: BackendKind::Linux,
        program: tools.systemd_run.clone(),
        arguments,
        current_directory: request.workspace.clone(),
        environment: BTreeMap::new(),
        clear_environment: true,
        cgroup_unit: Some(unit),
        cleanup_program: Some(tools.systemctl.clone()),
    })
}

fn launch_plan(
    plan: PreparedLaunch,
    stdout: Stdio,
    stderr: Stdio,
) -> Result<SandboxProcess, SandboxError> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt as _;

    let mut command = std::process::Command::new(&plan.program);
    command
        .args(&plan.arguments)
        .current_dir(&plan.current_directory)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    if plan.clear_environment {
        command.env_clear();
    }
    command.envs(&plan.environment);
    #[cfg(unix)]
    command.process_group(0);
    let child = command.spawn().map_err(|source| SandboxError::Launch {
        program: plan.program.clone(),
        source,
    })?;
    Ok(SandboxProcess {
        child,
        backend: plan.backend,
        cgroup_unit: plan.cgroup_unit,
        cleanup_program: plan.cleanup_program,
    })
}

fn inspect_process(process: &mut SandboxProcess) -> Result<ProcessInspection, SandboxError> {
    Ok(ProcessInspection {
        pid: process.id(),
        exit_status: process.child.try_wait()?,
        cgroup_unit: process.cgroup_unit.clone(),
    })
}

fn signal_process(process: &SandboxProcess, signal: Signal) -> Result<(), SandboxError> {
    let pid = i32::try_from(process.id()).map_err(|_| SandboxError::InvalidProcessId)?;
    kill(Pid::from_raw(pid), signal).map_err(|source| SandboxError::Signal {
        pid: process.id(),
        source,
    })
}

fn terminate_process(process: &mut SandboxProcess) -> Result<(), SandboxError> {
    if process.child.try_wait()?.is_none() {
        process.child.kill()?;
    }
    Ok(())
}

fn cleanup_process(process: &mut SandboxProcess) -> Result<(), SandboxError> {
    terminate_process(process)?;
    process.child.wait()?;
    Ok(())
}

fn control_linux_unit(process: &SandboxProcess, signal: &str) -> Result<(), SandboxError> {
    let program = process
        .cleanup_program
        .as_ref()
        .ok_or(SandboxError::MissingCleanupIdentity)?;
    let unit = process
        .cgroup_unit
        .as_ref()
        .ok_or(SandboxError::MissingCleanupIdentity)?;
    let status = std::process::Command::new(program)
        .args(["kill", "--kill-whom=all", "--signal", signal, unit])
        .env_clear()
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(SandboxError::ControlFailed {
            operation: "signal",
            unit: unit.clone(),
        })
    }
}

fn stop_linux_unit(process: &mut SandboxProcess) -> Result<(), SandboxError> {
    let program = process
        .cleanup_program
        .as_ref()
        .ok_or(SandboxError::MissingCleanupIdentity)?;
    let unit = process
        .cgroup_unit
        .as_ref()
        .ok_or(SandboxError::MissingCleanupIdentity)?;
    let status = std::process::Command::new(program)
        .args(["stop", unit])
        .env_clear()
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(SandboxError::ControlFailed {
            operation: "stop",
            unit: unit.clone(),
        })
    }
}

#[cfg(not(target_os = "linux"))]
fn discover_linux_backend() -> LinuxBackend {
    LinuxBackend {
        capabilities: BackendCapabilities {
            backend: BackendKind::Linux,
            status: BackendStatus::Unsupported,
            diagnostics: vec![CapabilityDiagnostic {
                code: UnsupportedCode::PlatformNotLinux,
                facility: "operating_system".to_owned(),
                detail: format!(
                    "Linux isolation requires Linux; current OS is {}",
                    std::env::consts::OS
                ),
            }],
        },
        tools: None,
    }
}

#[cfg(target_os = "linux")]
fn discover_linux_backend() -> LinuxBackend {
    let mut diagnostics = Vec::new();
    match effective_uid_from_proc() {
        Ok(0) => {}
        Ok(uid) => diagnostics.push(CapabilityDiagnostic {
            code: UnsupportedCode::PrivilegeRequired,
            facility: "user_namespace".to_owned(),
            detail: format!(
                "this host blocks the required unprivileged user namespace; run through an explicitly privileged supervisor (effective UID is {uid})"
            ),
        }),
        Err(detail) => diagnostics.push(CapabilityDiagnostic {
            code: UnsupportedCode::KernelProbeFailed,
            facility: "effective_uid".to_owned(),
            detail,
        }),
    }

    let tool_candidates = [
        ("systemd-run", "/usr/bin/systemd-run"),
        ("bwrap", "/usr/bin/bwrap"),
        ("systemctl", "/usr/bin/systemctl"),
    ];
    for (name, path) in tool_candidates {
        if canonical_regular_executable(Path::new(path), "linux_tool", true).is_err() {
            diagnostics.push(CapabilityDiagnostic {
                code: UnsupportedCode::MissingTool,
                facility: name.to_owned(),
                detail: format!("required trusted executable is unavailable at {path}"),
            });
        }
    }

    let cgroup_path = Path::new("/sys/fs/cgroup/cgroup.controllers");
    match fs::read_to_string(cgroup_path) {
        Ok(controllers) => {
            for required in ["cpu", "memory", "pids"] {
                if !controllers
                    .split_whitespace()
                    .any(|value| value == required)
                {
                    diagnostics.push(CapabilityDiagnostic {
                        code: UnsupportedCode::MissingCgroupController,
                        facility: format!("cgroup_v2.{required}"),
                        detail: format!("required cgroup v2 controller {required} is unavailable"),
                    });
                }
            }
        }
        Err(error) => diagnostics.push(CapabilityDiagnostic {
            code: UnsupportedCode::MissingCgroupV2,
            facility: "cgroup_v2".to_owned(),
            detail: format!("{} cannot be read: {error}", cgroup_path.display()),
        }),
    }

    if !matches!(std::env::consts::ARCH, "x86_64" | "aarch64") {
        diagnostics.push(CapabilityDiagnostic {
            code: UnsupportedCode::UnsupportedArchitecture,
            facility: "seccomp".to_owned(),
            detail: format!(
                "seccomp profile v1 is not compiled for {}",
                std::env::consts::ARCH
            ),
        });
    }
    let tools = if diagnostics.is_empty() {
        LinuxTools::new(
            "/usr/bin/systemd-run",
            "/usr/bin/bwrap",
            "/usr/bin/systemctl",
        )
        .ok()
    } else {
        None
    };
    LinuxBackend {
        capabilities: BackendCapabilities {
            backend: BackendKind::Linux,
            status: if diagnostics.is_empty() {
                BackendStatus::Supported
            } else {
                BackendStatus::Unsupported
            },
            diagnostics,
        },
        tools,
    }
}

#[cfg(target_os = "linux")]
fn effective_uid_from_proc() -> Result<u32, String> {
    let status = fs::read_to_string("/proc/self/status")
        .map_err(|error| format!("/proc/self/status cannot be read: {error}"))?;
    let line = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .ok_or_else(|| "Uid field is missing from /proc/self/status".to_owned())?;
    let effective = line
        .split_whitespace()
        .nth(2)
        .ok_or_else(|| "effective Uid is missing from /proc/self/status".to_owned())?;
    effective
        .parse()
        .map_err(|error| format!("effective Uid is invalid: {error}"))
}

fn validate_environment(environment: &BTreeMap<String, String>) -> Result<(), SandboxError> {
    if environment.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(SandboxError::TooManyEnvironmentEntries);
    }
    for (key, value) in environment {
        let valid_key = key.starts_with("EPOCH_")
            && key
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
            && !sensitive_environment_name(key);
        if !valid_key {
            return Err(SandboxError::InvalidEnvironment { key: key.clone() });
        }
        if value.len() > MAX_ENVIRONMENT_VALUE_BYTES || value.contains('\0') {
            return Err(SandboxError::InvalidEnvironment { key: key.clone() });
        }
    }
    Ok(())
}

fn sensitive_environment_name(key: &str) -> bool {
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL", "AUTH"]
        .iter()
        .any(|marker| key.contains(marker))
}

fn canonical_regular_executable(
    requested: &Path,
    field: &'static str,
    reject_symlinks: bool,
) -> Result<PathBuf, SandboxError> {
    if reject_symlinks
        && fs::symlink_metadata(requested).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(SandboxError::SymlinkRejected {
            field,
            path: requested.to_path_buf(),
        });
    }
    let canonical = fs::canonicalize(requested).map_err(|source| SandboxError::Path {
        field,
        path: requested.to_path_buf(),
        source,
    })?;
    if reject_symlinks {
        reject_symlink_components(&canonical, field)?;
    }
    let metadata = fs::metadata(&canonical).map_err(|source| SandboxError::Path {
        field,
        path: canonical.clone(),
        source,
    })?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(SandboxError::InvalidExecutable {
            field,
            path: canonical,
        });
    }
    Ok(canonical)
}

fn canonical_directory(
    requested: &Path,
    field: &'static str,
    reject_symlinks: bool,
) -> Result<PathBuf, SandboxError> {
    if reject_symlinks
        && fs::symlink_metadata(requested).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(SandboxError::SymlinkRejected {
            field,
            path: requested.to_path_buf(),
        });
    }
    let canonical = fs::canonicalize(requested).map_err(|source| SandboxError::Path {
        field,
        path: requested.to_path_buf(),
        source,
    })?;
    if reject_symlinks {
        reject_symlink_components(&canonical, field)?;
    }
    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err(SandboxError::InvalidDirectory {
            field,
            path: canonical,
        })
    }
}

fn reject_symlink_components(path: &Path, field: &'static str) -> Result<(), SandboxError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(SandboxError::InvalidPathComponent {
                    field,
                    path: absolute,
                });
            }
        }
        if fs::symlink_metadata(&current).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(SandboxError::SymlinkRejected {
                field,
                path: current,
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

fn validate_absolute_tool(path: &Path) -> Result<PathBuf, SandboxError> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().contains(&0)
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        Err(SandboxError::InvalidToolPath {
            path: path.to_path_buf(),
        })
    } else {
        Ok(path.to_path_buf())
    }
}

fn validate_unit_name(unit_name: &str) -> Result<(), SandboxError> {
    if unit_name.len() > 63
        || !unit_name.starts_with("epoch-")
        || !unit_name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        Err(SandboxError::InvalidUnitName)
    } else {
        Ok(())
    }
}

/// Sandbox validation, lifecycle, or host-control error.
#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("invalid resource limit: {field}")]
    InvalidLimit { field: &'static str },
    #[error("too many arguments: {actual}; maximum is {maximum}")]
    TooManyArguments { actual: usize, maximum: usize },
    #[error("argument {index} is invalid")]
    InvalidArgument { index: usize },
    #[error("too many environment entries")]
    TooManyEnvironmentEntries,
    #[error("environment entry is forbidden: {key}")]
    InvalidEnvironment { key: String },
    #[error("{field} path failed for {path}: {source}")]
    Path {
        field: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{field} is not a regular executable: {path}")]
    InvalidExecutable { field: &'static str, path: PathBuf },
    #[error("{field} is not a directory: {path}")]
    InvalidDirectory { field: &'static str, path: PathBuf },
    #[error("{field} contains a forbidden path component: {path}")]
    InvalidPathComponent { field: &'static str, path: PathBuf },
    #[error("{field} contains a symlink: {path}")]
    SymlinkRejected { field: &'static str, path: PathBuf },
    #[error("working directory {path} is outside workspace {workspace}")]
    PathOutsideWorkspace { path: PathBuf, workspace: PathBuf },
    #[error("invalid trusted tool path: {path}")]
    InvalidToolPath { path: PathBuf },
    #[error("invalid transient cgroup unit name")]
    InvalidUnitName,
    #[error("backend is unsupported on this host")]
    BackendUnsupported,
    #[error("failed to launch {program}: {source}")]
    Launch {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid child process identifier")]
    InvalidProcessId,
    #[error("failed to signal process {pid}: {source}")]
    Signal { pid: u32, source: nix::Error },
    #[error("Linux sandbox process has no cgroup cleanup identity")]
    MissingCleanupIdentity,
    #[error("cgroup {operation} failed for {unit}")]
    ControlFailed {
        operation: &'static str,
        unit: String,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
