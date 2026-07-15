use std::{env, path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use serde::Serialize;

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
    /// Inspect host support for Epoch's execution mechanisms.
    Doctor {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
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
    let cli = Cli::parse();
    match cli.command {
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
    fn command_tree_exposes_the_complete_runtime_spec_surface() {
        let command = Cli::command();
        let actual = command
            .get_subcommands()
            .map(|subcommand| subcommand.get_name())
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
                .map(|subcommand| subcommand.get_name())
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
}
