use std::{env, path::PathBuf, process::ExitCode, str::FromStr as _};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use epoch_core::{BranchId, EpochId, SessionId};
use epoch_supervisor::{
    AgentTermination, ApplicationRestoreMode, DirectSupervisor, EventPageRequest, InspectionError,
    RecoveryCode, RecoveryIssue, RecoveryOutcome, RunOutcome, SessionStatusReport,
};
use serde::Serialize;
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
    /// Run a reproducible fault scenario.
    Fault {
        #[command(subcommand)]
        command: FaultCommand,
    },
    /// Serve the local read-only inspection API.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: String,
    },
    /// Run the complete deterministic interview demonstration.
    Demo,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum RestoreMode {
    #[default]
    Strict,
    Inspect,
    ForkOnDivergence,
}

#[derive(Debug, Subcommand)]
enum BranchCommand {
    Promote { branch: String },
    Abandon { branch: String },
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
}

#[derive(Debug, Subcommand)]
enum EffectsCommand {
    List { session: String },
    Resolve(ResolveEffect),
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
    Run { suite: String },
    Report { run: String },
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

impl HostCapabilities {
    fn detect() -> Self {
        let linux = cfg!(target_os = "linux");
        Self {
            os: env::consts::OS,
            architecture: env::consts::ARCH,
            control_plane: Support::Available,
            linux_execution: linux.into(),
            procfs: (linux && std::path::Path::new("/proc/self/status").is_file()).into(),
            cgroup_v2: (linux
                && std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").is_file())
            .into(),
            overlayfs: (linux && filesystem_lists("overlay", "/proc/filesystems")).into(),
            kvm: (linux && std::path::Path::new("/dev/kvm").exists()).into(),
            criu: find_in_path("criu"),
            strace: find_in_path("strace"),
            perf: find_in_path("perf"),
            unshare: find_in_path("unshare"),
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
        Command::Run { manifest } => run_manifest(&manifest),
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
        Command::Restore { epoch, mode } => restore_application(&epoch, mode),
        Command::Diff {
            left,
            right,
            json: _,
        } => diff_application_epochs(&left, &right),
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

fn restore_application(epoch: &str, mode: RestoreMode) -> ExitCode {
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
    emit_recovery("restore", supervisor.restore_application(epoch_id, mode))
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
            Self::Fault { .. } => "fault",
            Self::Serve { .. } => "serve",
            Self::Demo => "demo",
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
            capabilities.backends.application_checkpoint.status,
            BackendStatus::Supported
        );
        assert!(capabilities.backends.application_checkpoint.registered);

        for backend in [
            &capabilities.backends.process_checkpoint,
            &capabilities.backends.criu_checkpoint,
            &capabilities.backends.workspace_checkpoint,
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
            ("branch", ["abandon", "promote"].as_slice()),
            ("capability", ["grant", "revoke"].as_slice()),
            ("effects", ["list", "resolve"].as_slice()),
            ("bench", ["report", "run"].as_slice()),
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
