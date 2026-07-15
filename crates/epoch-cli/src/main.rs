mod bench;
mod demo;
mod ml;

use std::{env, path::PathBuf, process::ExitCode, str::FromStr as _, sync::Arc};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use epoch_capabilities::{CapabilityConstraints, CapabilityError, CapabilityService, IssueRequest};
use epoch_core::{BranchId, CapabilityId, EpochId, SessionId};
use epoch_dashboard::{DashboardConfig, DashboardError, parse_loopback_bind, serve};
use epoch_effects::{DenyAllAuthorizer, DeterministicLocalDispatcher, EffectGateway};
use epoch_sandbox::{
    BackendCapabilities as SandboxBackendCapabilities, ExecutionBackend as _, LinuxBackend,
};
use epoch_storage::Store;
use epoch_supervisor::{
    AgentTermination, ApplicationRestoreMode, DirectSupervisor, EventPageRequest, InspectionError,
    RecoveryCode, RecoveryIssue, RecoveryOutcome, RunOutcome, SessionStatusReport,
};
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use serde_json::json;

const SUPERVISOR_FAILURE_EXIT: u8 = 125;
const RECOVERY_UNSUPPORTED_EXIT: u8 = 3;

#[derive(Debug, Parser)]
#[command(
    name = "epoch",
    version,
    about = "Secure, recoverable execution experiments for AI agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize an Epoch state directory.
    Init,
    /// Inspect host support for Epoch's execution mechanisms.
    Doctor {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run a workload under the selected execution backend.
    Run {
        /// Workload manifest to execute.
        #[arg(long)]
        manifest: PathBuf,
        /// Execution boundary to select explicitly. Linux never falls back to direct execution.
        #[arg(long, value_enum, default_value_t = ExecutionBackendSelection::Direct)]
        backend: ExecutionBackendSelection,
    },
    /// Show the current state of a session.
    Status { session: String },
    /// List the typed event timeline for a session or branch.
    Events {
        session: String,
        #[arg(long)]
        branch: Option<String>,
        /// Number of events to skip in stable branch/sequence order.
        #[arg(long, default_value_t = 0)]
        offset: u64,
        /// Maximum number of events to return (1 through 1000).
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Commit a composite execution checkpoint.
    Checkpoint {
        session: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        label: Option<String>,
    },
    /// Restore a committed epoch.
    Restore {
        epoch: String,
        #[arg(long, value_enum, default_value_t = RestoreMode::Strict)]
        mode: RestoreMode,
        /// New directory where the checkpointed workspace is published without clobbering.
        #[arg(long)]
        workspace_target: Option<PathBuf>,
    },
    /// Create a new logical branch from an epoch.
    Fork {
        epoch: String,
        #[arg(long)]
        name: String,
    },
    /// Suspend a branch at a safe boundary.
    Suspend { branch: String },
    /// Resume a suspended branch.
    Resume { branch: String },
    /// Manage branch promotion and abandonment.
    Branch {
        #[command(subcommand)]
        command: BranchCommand,
    },
    /// Compare two epochs or branches semantically.
    Diff {
        left: String,
        right: String,
        #[arg(long)]
        json: bool,
    },
    /// Manage branch-bound capabilities.
    Capability {
        #[command(subcommand)]
        command: CapabilityCommand,
    },
    /// Inspect and reconcile external effects.
    Effects {
        #[command(subcommand)]
        command: EffectsCommand,
    },
    /// Run and report benchmark suites.
    Bench {
        #[command(subcommand)]
        command: BenchCommand,
    },
    /// Export trajectories and run advisory learned-policy workflows.
    Ml {
        #[command(subcommand)]
        command: MlCommand,
    },
    /// Run a reproducible fault scenario.
    Fault {
        #[command(subcommand)]
        command: FaultCommand,
    },
    /// Serve the local read-only inspection API.
    Serve {
        /// Existing Epoch state directory containing state.db.
        #[arg(long, default_value = ".epoch")]
        state_root: PathBuf,
        /// Optional directory containing bounded benchmark JSON reports.
        #[arg(long)]
        results_root: Option<PathBuf>,
        /// Loopback listener. Non-loopback addresses are always refused.
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: String,
    },
    /// Run the complete deterministic interview demonstration.
    Demo {
        /// Explicit deterministic test-agent executable.
        #[arg(long)]
        agent: PathBuf,
        /// Dedicated demo-owned root. Existing unowned content is refused.
        #[arg(long)]
        root: PathBuf,
        /// Demo-owned workspace base inside `root`.
        #[arg(long)]
        workspace: PathBuf,
        /// Emit the complete machine-readable report to stdout.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum RestoreMode {
    #[default]
    Strict,
    Inspect,
    ForkOnDivergence,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ExecutionBackendSelection {
    #[default]
    Direct,
    Linux,
}

#[derive(Debug, Subcommand)]
enum BranchCommand {
    Promote { branch: String },
    Abandon { branch: String },
    Inspect { branch: String },
}

#[derive(Debug, Subcommand)]
enum CapabilityCommand {
    Grant {
        branch: String,
        action: String,
        constraints: Option<String>,
    },
    Revoke {
        capability: String,
    },
    Inspect {
        capability: String,
    },
}

#[derive(Debug, Subcommand)]
enum EffectsCommand {
    List { session: String },
    Resolve(ResolveEffect),
}

#[derive(Debug, Subcommand)]
enum MlCommand {
    /// Export privacy-safe, metadata-only branch trajectories as JSONL.
    Export {
        /// Epoch trusted-state root containing state.db.
        #[arg(long, default_value = ".epoch")]
        state_root: PathBuf,
        /// Session whose complete branch group will be exported together.
        #[arg(long)]
        session: String,
        /// Stable lowercase task/repository group used to prevent split leakage.
        #[arg(long)]
        task_group: String,
        /// New private JSONL file; existing paths are never overwritten.
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = epoch_trajectory::DEFAULT_MAX_BRANCHES)]
        max_branches: usize,
        #[arg(long, default_value_t = epoch_trajectory::DEFAULT_MAX_EVENTS_PER_BRANCH)]
        max_events_per_branch: usize,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantConstraints {
    subject: String,
    resource: String,
    #[serde(default)]
    max_uses: Option<u64>,
    #[serde(default)]
    budget_units: Option<u64>,
    #[serde(default)]
    expires_at_unix_ms: Option<i64>,
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("resolution")
        .required(true)
        .multiple(false)
        .args(["committed", "failed", "compensate"])
))]
struct ResolveEffect {
    effect: String,
    #[arg(long)]
    committed: bool,
    #[arg(long)]
    failed: bool,
    #[arg(long)]
    compensate: bool,
}

#[derive(Debug, Subcommand)]
enum BenchCommand {
    Run {
        suite: String,
        #[arg(long, default_value = ".epoch/benchmarks")]
        root: PathBuf,
        #[arg(long, default_value_t = 1)]
        warmups: u32,
        #[arg(long, default_value_t = 5)]
        repetitions: u32,
        #[arg(long, default_value_t = 1_048_576)]
        fixture_bytes: u64,
        #[arg(long, default_value_t = 16)]
        fixture_files: u32,
        #[arg(long, default_value_t = 24_301)]
        seed: u64,
        #[arg(long, default_value_t = 33_554_432)]
        cow_allocation_bytes: u64,
        #[arg(long, default_value_t = 2)]
        cow_children: u32,
        #[arg(long, default_value_t = 2_500)]
        cow_dirty_basis_points: u32,
        #[arg(long, default_value_t = 3)]
        cow_repetitions: u32,
        #[arg(long, default_value_t = 3)]
        performance_repetitions: u16,
        #[arg(long, default_value_t = 5)]
        isolation_repetitions: u16,
        #[arg(long, default_value_t = 4 * 1024 * 1024 * 1024_u64)]
        performance_max_memory_bytes: u64,
        #[arg(long)]
        performance_sandbox_helper: Option<PathBuf>,
        #[arg(long)]
        performance_probe: Option<PathBuf>,
        #[arg(long)]
        performance_workspace: Option<PathBuf>,
    },
    Report {
        run: String,
        #[arg(long, default_value = ".epoch/benchmarks")]
        root: PathBuf,
        #[arg(long, value_enum, default_value_t = BenchFormat::Markdown)]
        format: BenchFormat,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BenchFormat {
    Markdown,
    Json,
    Csv,
}

#[derive(Debug, Subcommand)]
enum FaultCommand {
    Run { scenario: String },
}

#[derive(Debug, Serialize)]
struct HostCapabilities {
    os: &'static str,
    architecture: &'static str,
    control_plane: Support,
    backends: BackendCapabilities,
    linux_execution: Support,
    procfs: Support,
    cgroup_v2: Support,
    overlayfs: Support,
    kvm: Support,
    criu: Option<PathBuf>,
    strace: Option<PathBuf>,
    perf: Option<PathBuf>,
    unshare: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct BackendCapabilities {
    direct_execution: BackendCapability,
    linux_isolation: SandboxBackendCapabilities,
    application_checkpoint: BackendCapability,
    process_checkpoint: BackendCapability,
    criu_checkpoint: BackendCapability,
    workspace_checkpoint: BackendCapability,
}

#[derive(Debug, Serialize)]
struct BackendCapability {
    status: BackendStatus,
    registered: bool,
    backend: Option<&'static str>,
    scope: &'static str,
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependency_detected: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BackendStatus {
    Supported,
    Unsupported,
}

#[derive(Debug, Serialize)]
struct RunReport {
    session_id: String,
    branch_id: String,
    termination: &'static str,
    exit_code: Option<i32>,
    signal: Option<i32>,
    protocol_records: usize,
    stderr_bytes: usize,
}

#[derive(Debug, Serialize)]
struct IntegratedStatusReport {
    #[serde(flatten)]
    execution: SessionStatusReport,
    application: serde_json::Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Support {
    Available,
    Unavailable,
}

impl From<bool> for Support {
    fn from(value: bool) -> Self {
        if value {
            Self::Available
        } else {
            Self::Unavailable
        }
    }
}

impl std::fmt::Display for Support {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available => formatter.write_str("available"),
            Self::Unavailable => formatter.write_str("unavailable"),
        }
    }
}

impl std::fmt::Display for BackendStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Supported => formatter.write_str("supported"),
            Self::Unsupported => formatter.write_str("unsupported"),
        }
    }
}

impl HostCapabilities {
    fn detect() -> Self {
        let linux = cfg!(target_os = "linux");
        let criu = find_in_path("criu");
        Self {
            os: env::consts::OS,
            architecture: env::consts::ARCH,
            control_plane: Support::Available,
            backends: BackendCapabilities::detect(criu.is_some()),
            linux_execution: linux.into(),
            procfs: (linux && std::path::Path::new("/proc/self/status").is_file()).into(),
            cgroup_v2: (linux
                && std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").is_file())
            .into(),
            overlayfs: (linux && filesystem_lists("overlay", "/proc/filesystems")).into(),
            kvm: (linux && std::path::Path::new("/dev/kvm").exists()).into(),
            criu,
            strace: find_in_path("strace"),
            perf: find_in_path("perf"),
            unshare: find_in_path("unshare"),
        }
    }
}

impl BackendCapabilities {
    fn detect(criu_dependency_detected: bool) -> Self {
        Self {
            direct_execution: BackendCapability {
                status: BackendStatus::Supported,
                registered: true,
                backend: Some("direct-process-v1"),
                scope: "process_lifecycle",
                reason: "the direct process supervisor is compiled and registered",
                dependency_detected: None,
            },
            linux_isolation: LinuxBackend::discover().capabilities(),
            application_checkpoint: BackendCapability {
                status: BackendStatus::Supported,
                registered: true,
                backend: Some("cooperative-w02-v1"),
                scope: "application_context_only",
                reason: "the cooperative W02 application checkpoint backend is registered",
                dependency_detected: None,
            },
            process_checkpoint: BackendCapability {
                status: BackendStatus::Unsupported,
                registered: false,
                backend: None,
                scope: "process_memory",
                reason: "no process checkpoint backend is registered",
                dependency_detected: None,
            },
            criu_checkpoint: BackendCapability {
                status: BackendStatus::Unsupported,
                registered: false,
                backend: None,
                scope: "process_tree",
                reason: "CRIU integration is not registered; tool presence alone is insufficient",
                dependency_detected: Some(criu_dependency_detected),
            },
            workspace_checkpoint: BackendCapability {
                status: BackendStatus::Supported,
                registered: true,
                backend: Some("full-copy-cas-v1"),
                scope: "workspace_files_without_process_memory",
                reason: "the deterministic full-copy CAS workspace backend is registered",
                dependency_detected: None,
            },
        }
    }
}

fn filesystem_lists(name: &str, source: &str) -> bool {
    std::fs::read_to_string(source).is_ok_and(|contents| {
        contents
            .lines()
            .any(|line| line.split_whitespace().last() == Some(name))
    })
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|path| path.join(binary))
        .find(|candidate| candidate.is_file())
}

fn main() -> ExitCode {
    execute(Cli::parse().command)
}

fn execute(command: Command) -> ExitCode {
    match command {
        Command::Run { manifest, backend } => run_selected_backend(&manifest, backend),
        Command::Status { session } => inspect_status(&session),
        Command::Events {
            session,
            branch,
            offset,
            limit,
        } => inspect_events(&session, branch.as_deref(), offset, limit),
        Command::Checkpoint {
            session,
            branch,
            label,
        } => checkpoint_application(&session, branch.as_deref(), label.as_deref()),
        Command::Restore {
            epoch,
            mode,
            workspace_target,
        } => restore_application(&epoch, mode, workspace_target.as_deref()),
        Command::Diff {
            left,
            right,
            json: _,
        } => diff_application_epochs(&left, &right),
        Command::Fork { epoch, name } => fork_application_epoch(&epoch, &name),
        Command::Branch { command } => execute_branch_command(command),
        Command::Capability { command } => execute_capability_command(command),
        Command::Effects { command } => execute_effects_command(command),
        Command::Demo {
            agent,
            root,
            workspace,
            json,
        } => demo::run(&demo::DemoConfig {
            agent,
            root,
            workspace,
            json,
        }),
        Command::Bench { command } => execute_bench(command),
        Command::Ml { command } => execute_ml_command(command),
        Command::Serve {
            state_root,
            results_root,
            bind,
        } => serve_dashboard(state_root, results_root, &bind),
        Command::Doctor { json } => {
            let capabilities = HostCapabilities::detect();
            if json {
                match serde_json::to_string_pretty(&capabilities) {
                    Ok(output) => println!("{output}"),
                    Err(error) => {
                        eprintln!("failed to serialize diagnostics: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                println!("Epoch host diagnostics");
                println!("  host: {}/{}", capabilities.os, capabilities.architecture);
                println!("  control plane: {}", capabilities.control_plane);
                println!(
                    "  direct execution backend: {}",
                    capabilities.backends.direct_execution.status
                );
                println!(
                    "  application checkpoint backend: {}",
                    capabilities.backends.application_checkpoint.status
                );
                println!(
                    "  process checkpoint backend: {}",
                    capabilities.backends.process_checkpoint.status
                );
                println!(
                    "  CRIU checkpoint backend: {}",
                    capabilities.backends.criu_checkpoint.status
                );
                println!(
                    "  workspace checkpoint backend: {}",
                    capabilities.backends.workspace_checkpoint.status
                );
                println!("  Linux execution: {}", capabilities.linux_execution);
                println!("  procfs: {}", capabilities.procfs);
                println!("  cgroup v2: {}", capabilities.cgroup_v2);
                println!("  OverlayFS: {}", capabilities.overlayfs);
                println!("  KVM: {}", capabilities.kvm);
                println!("  CRIU: {}", display_path(capabilities.criu.as_ref()));
                println!("  strace: {}", display_path(capabilities.strace.as_ref()));
                println!("  perf: {}", display_path(capabilities.perf.as_ref()));
                println!("  unshare: {}", display_path(capabilities.unshare.as_ref()));
                if capabilities.linux_execution == Support::Unavailable {
                    println!(
                        "\nThis host can build the control plane, but real isolation and checkpoint tests require Linux."
                    );
                }
            }
            ExitCode::SUCCESS
        }
        unfinished => {
            eprintln!("epoch {} is not implemented yet", unfinished.command_path());
            ExitCode::from(2)
        }
    }
}

fn execute_ml_command(command: MlCommand) -> ExitCode {
    match command {
        MlCommand::Export {
            state_root,
            session,
            task_group,
            output,
            max_branches,
            max_events_per_branch,
        } => ml::export(&ml::ExportOptions {
            state_root,
            session,
            task_group,
            output,
            max_branches,
            max_events_per_branch,
        }),
    }
}

fn serve_dashboard(state_root: PathBuf, results_root: Option<PathBuf>, raw_bind: &str) -> ExitCode {
    let bind = match parse_loopback_bind(raw_bind) {
        Ok(bind) => bind,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    match serve(DashboardConfig {
        state_root,
        results_root,
        bind,
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_dashboard_error(&error),
    }
}

fn report_dashboard_error(error: &DashboardError) -> ExitCode {
    if error.is_user_error() {
        eprintln!("{error}");
        ExitCode::from(2)
    } else {
        eprintln!("trusted dashboard state is unavailable: {error}");
        ExitCode::from(SUPERVISOR_FAILURE_EXIT)
    }
}

fn execute_capability_command(command: CapabilityCommand) -> ExitCode {
    match command {
        CapabilityCommand::Grant {
            branch,
            action,
            constraints,
        } => grant_capability(&branch, &action, constraints.as_deref()),
        CapabilityCommand::Revoke { capability } => revoke_capability(&capability),
        CapabilityCommand::Inspect { capability } => inspect_capability(&capability),
    }
}

fn grant_capability(branch: &str, action: &str, constraints: Option<&str>) -> ExitCode {
    let branch_id = match BranchId::from_str(branch) {
        Ok(branch_id) => branch_id,
        Err(error) => return user_security_error(format!("invalid branch ID: {error}")),
    };
    let constraints = match constraints {
        Some(value) => match serde_json::from_str::<GrantConstraints>(value) {
            Ok(constraints) => constraints,
            Err(error) => {
                return user_security_error(format!("invalid capability constraints: {error}"));
            }
        },
        None => {
            return user_security_error(
                "capability constraints JSON must include subject and resource",
            );
        }
    };
    let database = match existing_database_path() {
        Ok(database) => database,
        Err(exit) => return exit,
    };
    let store = match Store::open(&database) {
        Ok(store) => store,
        Err(error) => return trusted_security_error(error.to_string()),
    };
    let branch_context = match store
        .connection()
        .query_row(
            "SELECT b.session_id, s.policy_revision \
             FROM branches b JOIN sessions s ON s.id = b.session_id WHERE b.id = ?1",
            [branch_id.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
    {
        Ok(Some(context)) => context,
        Ok(None) => return user_security_error(format!("branch {branch_id} does not exist")),
        Err(error) => return trusted_security_error(error.to_string()),
    };
    let Ok(session_id) = SessionId::from_str(&branch_context.0) else {
        return trusted_security_error("stored session ID is invalid");
    };
    let Ok(policy_revision) = u64::try_from(branch_context.1) else {
        return trusted_security_error("stored policy revision is invalid");
    };
    drop(store);

    let service = match CapabilityService::open(&database) {
        Ok(service) => service,
        Err(error) => return capability_error(&error),
    };
    if let Err(error) = service.set_policy_revision(session_id, branch_id, policy_revision) {
        return capability_error(&error);
    }
    let issued = match service.issue(&IssueRequest {
        session_id,
        branch_id,
        subject: constraints.subject,
        action: action.to_owned(),
        resource: constraints.resource,
        constraints: CapabilityConstraints {
            max_uses: constraints.max_uses,
            budget_units: constraints.budget_units,
        },
        expires_at_unix_ms: constraints.expires_at_unix_ms,
        policy_revision,
    }) {
        Ok(issued) => issued,
        Err(error) => return capability_error(&error),
    };
    let snapshot = match service.inspect(issued.capability_id) {
        Ok(snapshot) => snapshot,
        Err(error) => return capability_error(&error),
    };
    print_json(&json!({
        "capability_id": snapshot.capability_id,
        "session_id": snapshot.session_id,
        "branch_id": snapshot.branch_id,
        "subject": snapshot.subject,
        "action": snapshot.action,
        "resource": snapshot.resource,
        "remaining_uses": snapshot.remaining_uses,
        "remaining_budget_units": snapshot.remaining_budget_units,
        "policy_revision": snapshot.policy_revision,
        "state": snapshot.state,
        "expires_at_unix_ms": snapshot.expires_at_unix_ms,
        "handle": issued.handle.expose(),
    }))
}

fn inspect_capability(capability: &str) -> ExitCode {
    let capability_id = match CapabilityId::from_str(capability) {
        Ok(capability_id) => capability_id,
        Err(error) => return user_security_error(format!("invalid capability ID: {error}")),
    };
    let service = match existing_capability_service() {
        Ok(service) => service,
        Err(exit) => return exit,
    };
    match service.inspect(capability_id) {
        Ok(snapshot) => print_json(&snapshot),
        Err(error) => capability_error(&error),
    }
}

fn revoke_capability(capability: &str) -> ExitCode {
    let capability_id = match CapabilityId::from_str(capability) {
        Ok(capability_id) => capability_id,
        Err(error) => return user_security_error(format!("invalid capability ID: {error}")),
    };
    let service = match existing_capability_service() {
        Ok(service) => service,
        Err(exit) => return exit,
    };
    if let Err(error) = service.revoke_by_id(capability_id) {
        return capability_error(&error);
    }
    match service.inspect(capability_id) {
        Ok(snapshot) => print_json(&snapshot),
        Err(error) => capability_error(&error),
    }
}

fn execute_effects_command(command: EffectsCommand) -> ExitCode {
    match command {
        EffectsCommand::List { session } => list_effects(&session),
        EffectsCommand::Resolve(_) => user_security_error(
            "effect resolution is not implemented; unresolved outcomes remain fail-closed",
        ),
    }
}

fn list_effects(session: &str) -> ExitCode {
    let session_id = match SessionId::from_str(session) {
        Ok(session_id) => session_id,
        Err(error) => return user_security_error(format!("invalid session ID: {error}")),
    };
    let database = match existing_database_path() {
        Ok(database) => database,
        Err(exit) => return exit,
    };
    let gateway = match EffectGateway::open(
        database,
        PathBuf::from(".epoch/blobs"),
        Arc::new(DenyAllAuthorizer),
        Arc::new(DeterministicLocalDispatcher::default()),
    ) {
        Ok(gateway) => gateway,
        Err(error) => return trusted_security_error(error.to_string()),
    };
    match gateway.list(session_id, None) {
        Ok(records) => print_json(&records),
        Err(error) => trusted_security_error(error.to_string()),
    }
}

fn existing_capability_service() -> Result<CapabilityService, ExitCode> {
    let database = existing_database_path()?;
    CapabilityService::open(database).map_err(|error| capability_error(&error))
}

fn existing_database_path() -> Result<PathBuf, ExitCode> {
    DirectSupervisor::open_existing(".epoch").map_err(|error| report_inspection_error(&error))?;
    Ok(PathBuf::from(".epoch/state.db"))
}

fn capability_error(error: &CapabilityError) -> ExitCode {
    if matches!(
        error,
        CapabilityError::CapabilityNotFound { .. }
            | CapabilityError::InvalidField { .. }
            | CapabilityError::InvalidExpiration
            | CapabilityError::PolicyNotInitialized
            | CapabilityError::PolicyRevisionRollback { .. }
            | CapabilityError::PolicyNotCurrent { .. }
    ) {
        user_security_error(error.to_string())
    } else {
        trusted_security_error(error.to_string())
    }
}

fn user_security_error(detail: impl AsRef<str>) -> ExitCode {
    eprintln!("{}", detail.as_ref());
    ExitCode::from(2)
}

fn trusted_security_error(detail: impl AsRef<str>) -> ExitCode {
    eprintln!("trusted state is unavailable: {}", detail.as_ref());
    ExitCode::from(SUPERVISOR_FAILURE_EXIT)
}

fn execute_bench(command: BenchCommand) -> ExitCode {
    match command {
        BenchCommand::Run {
            suite,
            root,
            warmups,
            repetitions,
            fixture_bytes,
            fixture_files,
            seed,
            cow_allocation_bytes,
            cow_children,
            cow_dirty_basis_points,
            cow_repetitions,
            performance_repetitions,
            isolation_repetitions,
            performance_max_memory_bytes,
            performance_sandbox_helper,
            performance_probe,
            performance_workspace,
        } => bench::run(&bench::RunOptions {
            suite,
            root,
            warmups,
            repetitions,
            fixture_bytes,
            fixture_files,
            seed,
            cow_allocation_bytes,
            cow_children,
            cow_dirty_basis_points,
            cow_repetitions,
            performance_repetitions,
            isolation_repetitions,
            performance_max_memory_bytes,
            performance_sandbox_helper,
            performance_probe,
            performance_workspace,
        }),
        BenchCommand::Report { run, root, format } => bench::report(&run, &root, format),
    }
}

fn inspect_status(raw_session: &str) -> ExitCode {
    let session_id = match parse_session_id(raw_session) {
        Ok(session_id) => session_id,
        Err(status) => return status,
    };
    let supervisor = match DirectSupervisor::open_existing(".epoch") {
        Ok(supervisor) => supervisor,
        Err(error) => return report_inspection_error(&error),
    };
    match supervisor.session_status(session_id) {
        Ok(execution) => print_json(&IntegratedStatusReport {
            execution,
            application: recovery_value(supervisor.application_status(session_id, None)),
        }),
        Err(error) => report_inspection_error(&error),
    }
}

fn inspect_events(
    raw_session: &str,
    raw_branch: Option<&str>,
    offset: u64,
    limit: usize,
) -> ExitCode {
    let session_id = match parse_session_id(raw_session) {
        Ok(session_id) => session_id,
        Err(status) => return status,
    };
    let branch_id = match raw_branch {
        Some(value) => {
            let Ok(branch_id) = value.parse::<BranchId>() else {
                eprintln!("invalid branch ID: {value:?}");
                return ExitCode::from(2);
            };
            Some(branch_id)
        }
        None => None,
    };
    let supervisor = match DirectSupervisor::open_existing(".epoch") {
        Ok(supervisor) => supervisor,
        Err(error) => return report_inspection_error(&error),
    };
    match supervisor.events(EventPageRequest {
        session_id,
        branch_id,
        offset,
        limit,
    }) {
        Ok(report) => print_json(&report),
        Err(error) => report_inspection_error(&error),
    }
}

fn parse_session_id(value: &str) -> Result<SessionId, ExitCode> {
    value.parse().map_err(|_| {
        eprintln!("invalid session ID: {value:?}");
        ExitCode::from(2)
    })
}

fn report_inspection_error(error: &InspectionError) -> ExitCode {
    eprintln!("{error}");
    if error.is_user_error() {
        ExitCode::from(2)
    } else {
        ExitCode::from(SUPERVISOR_FAILURE_EXIT)
    }
}

fn print_json(value: &impl Serialize) -> ExitCode {
    match serde_json::to_string(value) {
        Ok(encoded) => {
            println!("{encoded}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("failed to encode inspection report: {error}");
            ExitCode::from(SUPERVISOR_FAILURE_EXIT)
        }
    }
}

fn checkpoint_application(session: &str, branch: Option<&str>, label: Option<&str>) -> ExitCode {
    let session_id = match SessionId::from_str(session) {
        Ok(session_id) => session_id,
        Err(error) => {
            return emit_recovery_issue(
                "checkpoint",
                "failed",
                RecoveryCode::NotFound,
                format!("invalid session ID: {error}"),
                ExitCode::from(SUPERVISOR_FAILURE_EXIT),
            );
        }
    };
    let branch_id = match branch.map(BranchId::from_str).transpose() {
        Ok(branch_id) => branch_id,
        Err(error) => {
            return emit_recovery_issue(
                "checkpoint",
                "failed",
                RecoveryCode::NotFound,
                format!("invalid branch ID: {error}"),
                ExitCode::from(SUPERVISOR_FAILURE_EXIT),
            );
        }
    };
    let supervisor = match recovery_supervisor("checkpoint") {
        Ok(supervisor) => supervisor,
        Err(exit) => return exit,
    };
    emit_recovery(
        "checkpoint",
        supervisor.checkpoint_application(session_id, branch_id, label),
    )
}

fn restore_application(
    epoch: &str,
    mode: RestoreMode,
    workspace_target: Option<&std::path::Path>,
) -> ExitCode {
    let epoch_id = match EpochId::from_str(epoch) {
        Ok(epoch_id) => epoch_id,
        Err(error) => {
            return emit_recovery_issue(
                "restore",
                "failed",
                RecoveryCode::NotFound,
                format!("invalid epoch ID: {error}"),
                ExitCode::from(SUPERVISOR_FAILURE_EXIT),
            );
        }
    };
    let mode = match mode {
        RestoreMode::Strict => ApplicationRestoreMode::Activate,
        RestoreMode::Inspect => ApplicationRestoreMode::Inspect,
        RestoreMode::ForkOnDivergence => {
            return emit_recovery_issue(
                "restore",
                "unsupported",
                RecoveryCode::UnsupportedMode,
                "fork-on-divergence requires logical branching and is not application restore"
                    .to_owned(),
                ExitCode::from(RECOVERY_UNSUPPORTED_EXIT),
            );
        }
    };
    let supervisor = match recovery_supervisor("restore") {
        Ok(supervisor) => supervisor,
        Err(exit) => return exit,
    };
    emit_recovery(
        "restore",
        supervisor.restore_application(epoch_id, mode, workspace_target),
    )
}

fn diff_application_epochs(before: &str, after: &str) -> ExitCode {
    let before_epoch_id = match EpochId::from_str(before) {
        Ok(epoch_id) => epoch_id,
        Err(error) => {
            return emit_recovery_issue(
                "diff",
                "failed",
                RecoveryCode::NotFound,
                format!("invalid before epoch ID: {error}"),
                ExitCode::from(SUPERVISOR_FAILURE_EXIT),
            );
        }
    };
    let after_epoch_id = match EpochId::from_str(after) {
        Ok(epoch_id) => epoch_id,
        Err(error) => {
            return emit_recovery_issue(
                "diff",
                "failed",
                RecoveryCode::NotFound,
                format!("invalid after epoch ID: {error}"),
                ExitCode::from(SUPERVISOR_FAILURE_EXIT),
            );
        }
    };
    let supervisor = match recovery_supervisor("diff") {
        Ok(supervisor) => supervisor,
        Err(exit) => return exit,
    };
    emit_recovery(
        "diff",
        supervisor.diff_application_epochs(before_epoch_id, after_epoch_id),
    )
}

fn fork_application_epoch(epoch: &str, name: &str) -> ExitCode {
    let epoch_id = match EpochId::from_str(epoch) {
        Ok(epoch_id) => epoch_id,
        Err(error) => {
            return emit_recovery_issue(
                "fork",
                "failed",
                RecoveryCode::NotFound,
                format!("invalid epoch ID: {error}"),
                ExitCode::from(SUPERVISOR_FAILURE_EXIT),
            );
        }
    };
    let supervisor = match recovery_supervisor("fork") {
        Ok(supervisor) => supervisor,
        Err(exit) => return exit,
    };
    emit_recovery("fork", supervisor.fork_application_epoch(epoch_id, name))
}

fn execute_branch_command(command: BranchCommand) -> ExitCode {
    match command {
        BranchCommand::Inspect { branch } => inspect_fork_branch(&branch),
        BranchCommand::Promote { branch } => emit_recovery_issue(
            "branch.promote",
            "unsupported",
            RecoveryCode::UnsupportedMode,
            format!(
                "branch {branch} cannot be promoted until canonical-branch compare-and-swap is implemented"
            ),
            ExitCode::from(RECOVERY_UNSUPPORTED_EXIT),
        ),
        BranchCommand::Abandon { branch } => emit_recovery_issue(
            "branch.abandon",
            "unsupported",
            RecoveryCode::UnsupportedMode,
            format!("branch {branch} abandonment is not implemented"),
            ExitCode::from(RECOVERY_UNSUPPORTED_EXIT),
        ),
    }
}

fn inspect_fork_branch(branch: &str) -> ExitCode {
    let branch_id = match BranchId::from_str(branch) {
        Ok(branch_id) => branch_id,
        Err(error) => {
            return emit_recovery_issue(
                "branch.inspect",
                "failed",
                RecoveryCode::NotFound,
                format!("invalid branch ID: {error}"),
                ExitCode::from(SUPERVISOR_FAILURE_EXIT),
            );
        }
    };
    let supervisor = match recovery_supervisor("branch.inspect") {
        Ok(supervisor) => supervisor,
        Err(exit) => return exit,
    };
    emit_recovery("branch.inspect", supervisor.inspect_fork_branch(branch_id))
}

fn recovery_supervisor(operation: &str) -> Result<DirectSupervisor, ExitCode> {
    DirectSupervisor::open(".epoch").map_err(|error| {
        emit_recovery_issue(
            operation,
            "failed",
            RecoveryCode::Persistence,
            error.to_string(),
            ExitCode::from(SUPERVISOR_FAILURE_EXIT),
        )
    })
}

fn emit_recovery<T: Serialize>(operation: &str, outcome: RecoveryOutcome<T>) -> ExitCode {
    let exit = match &outcome {
        RecoveryOutcome::Supported(_) => ExitCode::SUCCESS,
        RecoveryOutcome::Unsupported(_) => ExitCode::from(RECOVERY_UNSUPPORTED_EXIT),
        RecoveryOutcome::Failed(_) => ExitCode::from(SUPERVISOR_FAILURE_EXIT),
    };
    let mut document = recovery_value(outcome);
    document["operation"] = json!(operation);
    emit_recovery_json(&document, exit)
}

fn recovery_value<T: Serialize>(outcome: RecoveryOutcome<T>) -> serde_json::Value {
    match outcome {
        RecoveryOutcome::Supported(result) => json!({
            "outcome": "supported",
            "result": result,
        }),
        RecoveryOutcome::Unsupported(issue) => json!({
            "outcome": "unsupported",
            "issue": issue,
        }),
        RecoveryOutcome::Failed(issue) => json!({
            "outcome": "failed",
            "issue": issue,
        }),
    }
}

fn emit_recovery_issue(
    operation: &str,
    outcome: &str,
    code: RecoveryCode,
    detail: String,
    exit: ExitCode,
) -> ExitCode {
    emit_recovery_json(
        &json!({
            "operation": operation,
            "outcome": outcome,
            "issue": RecoveryIssue { code, detail },
        }),
        exit,
    )
}

fn emit_recovery_json(document: &serde_json::Value, exit: ExitCode) -> ExitCode {
    match serde_json::to_string(document) {
        Ok(encoded) => {
            println!("{encoded}");
            exit
        }
        Err(error) => {
            eprintln!("failed to encode recovery report: {error}");
            ExitCode::from(SUPERVISOR_FAILURE_EXIT)
        }
    }
}

fn run_selected_backend(
    manifest: &std::path::Path,
    backend: ExecutionBackendSelection,
) -> ExitCode {
    match backend {
        ExecutionBackendSelection::Direct => run_manifest(manifest),
        ExecutionBackendSelection::Linux => {
            let capabilities = LinuxBackend::discover().capabilities();
            let detail = capabilities.diagnostics.first().map_or_else(
                || {
                    "the Linux boundary is available, but the direct supervisor launch adapter \
                     is not composed with it yet"
                        .to_owned()
                },
                |diagnostic| diagnostic.detail.clone(),
            );
            eprintln!("Linux execution was selected and cannot start: {detail}");
            ExitCode::from(RECOVERY_UNSUPPORTED_EXIT)
        }
    }
}

fn run_manifest(manifest: &std::path::Path) -> ExitCode {
    let supervisor = match DirectSupervisor::open(".epoch") {
        Ok(supervisor) => supervisor,
        Err(error) => {
            eprintln!("supervisor initialization failed: {error}");
            return ExitCode::from(SUPERVISOR_FAILURE_EXIT);
        }
    };
    match supervisor.run_manifest(manifest) {
        Ok(outcome) => report_run(&outcome),
        Err(error) => {
            eprintln!("supervisor failed: {error}");
            ExitCode::from(SUPERVISOR_FAILURE_EXIT)
        }
    }
}

fn report_run(outcome: &RunOutcome) -> ExitCode {
    let (termination, exit_code, signal, status) = match outcome.termination {
        AgentTermination::Succeeded { code } => ("succeeded", Some(code), None, ExitCode::SUCCESS),
        AgentTermination::NonZero { code, signal } => {
            ("nonzero", code, signal, nonzero_exit_code(code))
        }
    };
    let report = RunReport {
        session_id: outcome.session_id.to_string(),
        branch_id: outcome.branch_id.to_string(),
        termination,
        exit_code,
        signal,
        protocol_records: outcome.protocol_records,
        stderr_bytes: outcome.stderr.len(),
    };
    match serde_json::to_string(&report) {
        Ok(encoded) => println!("{encoded}"),
        Err(error) => {
            eprintln!("failed to encode run report: {error}");
            return ExitCode::from(SUPERVISOR_FAILURE_EXIT);
        }
    }
    status
}

fn nonzero_exit_code(code: Option<i32>) -> ExitCode {
    code.and_then(|value| u8::try_from(value).ok())
        .filter(|value| *value != 0)
        .map_or_else(|| ExitCode::from(1), ExitCode::from)
}

impl Command {
    const fn command_path(&self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Doctor { .. } => "doctor",
            Self::Run { .. } => "run",
            Self::Status { .. } => "status",
            Self::Events { .. } => "events",
            Self::Checkpoint { .. } => "checkpoint",
            Self::Restore { .. } => "restore",
            Self::Fork { .. } => "fork",
            Self::Suspend { .. } => "suspend",
            Self::Resume { .. } => "resume",
            Self::Branch { .. } => "branch",
            Self::Diff { .. } => "diff",
            Self::Capability { .. } => "capability",
            Self::Effects { .. } => "effects",
            Self::Bench { .. } => "bench",
            Self::Ml { .. } => "ml",
            Self::Fault { .. } => "fault",
            Self::Serve { .. } => "serve",
            Self::Demo { .. } => "demo",
        }
    }
}

fn display_path(path: Option<&PathBuf>) -> String {
    path.map_or_else(
        || "not found".to_owned(),
        |value| value.display().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::CommandFactory;

    use super::*;

    #[test]
    fn current_host_always_supports_control_plane() {
        assert_eq!(HostCapabilities::detect().control_plane, Support::Available);
    }

    #[test]
    fn support_display_is_unambiguous() {
        assert_eq!(Support::Available.to_string(), "available");
        assert_eq!(Support::Unavailable.to_string(), "unavailable");
    }

    #[test]
    fn backend_discovery_reports_only_registered_implementations_as_supported() {
        let capabilities = HostCapabilities::detect();

        assert_eq!(
            capabilities.backends.direct_execution.status,
            BackendStatus::Supported
        );
        assert!(capabilities.backends.direct_execution.registered);
        assert_eq!(
            capabilities.backends.linux_isolation.backend,
            epoch_sandbox::BackendKind::Linux
        );
        if capabilities.backends.linux_isolation.status == epoch_sandbox::BackendStatus::Unsupported
        {
            assert!(!capabilities.backends.linux_isolation.diagnostics.is_empty());
        }
        assert_eq!(
            capabilities.backends.application_checkpoint.status,
            BackendStatus::Supported
        );
        assert!(capabilities.backends.application_checkpoint.registered);

        assert_eq!(
            capabilities.backends.workspace_checkpoint.status,
            BackendStatus::Supported
        );
        assert!(capabilities.backends.workspace_checkpoint.registered);
        assert_eq!(
            capabilities.backends.workspace_checkpoint.backend,
            Some("full-copy-cas-v1")
        );

        for backend in [
            &capabilities.backends.process_checkpoint,
            &capabilities.backends.criu_checkpoint,
        ] {
            assert_eq!(backend.status, BackendStatus::Unsupported);
            assert!(!backend.registered);
            assert!(backend.backend.is_none());
        }
        assert_eq!(
            capabilities.backends.criu_checkpoint.dependency_detected,
            Some(capabilities.criu.is_some())
        );
    }

    #[test]
    fn command_tree_exposes_the_complete_runtime_spec_surface() {
        let command = Cli::command();
        let actual = command
            .get_subcommands()
            .map(clap::Command::get_name)
            .collect::<BTreeSet<_>>();
        let expected = [
            "bench",
            "branch",
            "capability",
            "checkpoint",
            "demo",
            "diff",
            "doctor",
            "effects",
            "events",
            "fault",
            "fork",
            "init",
            "ml",
            "restore",
            "resume",
            "run",
            "serve",
            "status",
            "suspend",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();

        assert_eq!(actual, expected);
    }

    #[test]
    fn nested_command_groups_match_the_runtime_spec() {
        let command = Cli::command();
        for (group, expected) in [
            ("branch", ["abandon", "inspect", "promote"].as_slice()),
            ("capability", ["grant", "inspect", "revoke"].as_slice()),
            ("effects", ["list", "resolve"].as_slice()),
            ("bench", ["report", "run"].as_slice()),
            ("ml", ["export"].as_slice()),
            ("fault", ["run"].as_slice()),
        ] {
            let subcommands = command
                .find_subcommand(group)
                .expect("command group exists")
                .get_subcommands()
                .map(clap::Command::get_name)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                subcommands,
                expected.iter().copied().collect(),
                "unexpected {group} command surface"
            );
        }
    }

    #[test]
    fn representative_spec_commands_parse() {
        for arguments in [
            vec!["epoch", "run", "--manifest", "workload.toml"],
            vec![
                "epoch",
                "run",
                "--backend",
                "linux",
                "--manifest",
                "workload.toml",
            ],
            vec!["epoch", "events", "session-1", "--branch", "branch-1"],
            vec![
                "epoch",
                "checkpoint",
                "session-1",
                "--branch",
                "branch-1",
                "--label",
                "before-edit",
            ],
            vec![
                "epoch",
                "restore",
                "epoch-1",
                "--mode",
                "fork-on-divergence",
            ],
            vec!["epoch", "effects", "resolve", "effect-1", "--committed"],
            vec!["epoch", "branch", "inspect", "branch-1"],
            vec![
                "epoch",
                "demo",
                "--agent",
                "/tmp/epoch-test-agent",
                "--root",
                "/tmp/epoch-demo",
                "--workspace",
                "/tmp/epoch-demo/workspaces",
                "--json",
            ],
            vec![
                "epoch",
                "bench",
                "run",
                "all",
                "--performance-probe",
                "/usr/local/libexec/epoch-performance-probe",
            ],
            vec!["epoch", "serve", "--bind", "127.0.0.1:9090"],
        ] {
            Cli::try_parse_from(arguments).expect("specified command must parse");
        }
    }

    #[test]
    fn unfinished_commands_return_an_explicit_failure() {
        assert_ne!(execute(Command::Init), ExitCode::SUCCESS);
    }

    #[test]
    fn effect_resolution_requires_exactly_one_outcome() {
        assert!(Cli::try_parse_from(["epoch", "effects", "resolve", "effect-1"]).is_err());
        assert!(
            Cli::try_parse_from([
                "epoch",
                "effects",
                "resolve",
                "effect-1",
                "--committed",
                "--failed",
            ])
            .is_err()
        );
    }
}
