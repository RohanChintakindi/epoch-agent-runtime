use std::env;

use epoch_sandbox::{ExecutionBackend as _, LinuxBackend};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct BackendReport {
    pub host_os: &'static str,
    pub architecture: &'static str,
    pub backends: Vec<BackendCard>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BackendCard {
    pub id: &'static str,
    pub status: &'static str,
    pub registered: bool,
    pub scope: &'static str,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_detected: Option<bool>,
}

#[must_use]
pub fn detect() -> BackendReport {
    let linux = LinuxBackend::discover().capabilities();
    let linux_status = match linux.status {
        epoch_sandbox::BackendStatus::Supported => "supported",
        epoch_sandbox::BackendStatus::Unsupported => "unsupported",
    };
    let linux_reason = if linux.diagnostics.is_empty() {
        "all required Linux isolation facilities passed discovery".to_owned()
    } else {
        linux
            .diagnostics
            .iter()
            .take(4)
            .map(|diagnostic| format!("{}: {}", diagnostic.facility, diagnostic.detail))
            .collect::<Vec<_>>()
            .join("; ")
    };
    BackendReport {
        host_os: env::consts::OS,
        architecture: env::consts::ARCH,
        backends: vec![
            BackendCard {
                id: "direct-process-v1",
                status: "supported",
                registered: true,
                scope: "process_lifecycle",
                reason: "direct process supervision is compiled and registered".to_owned(),
                dependency_detected: None,
            },
            BackendCard {
                id: "linux-isolation-v1",
                status: linux_status,
                registered: linux_status == "supported",
                scope: "namespaces_cgroups_seccomp",
                reason: linux_reason,
                dependency_detected: None,
            },
            BackendCard {
                id: "cooperative-w02-v1",
                status: "supported",
                registered: true,
                scope: "application_context",
                reason: "cooperative application checkpointing is registered".to_owned(),
                dependency_detected: None,
            },
            BackendCard {
                id: "full-copy-cas-v1",
                status: "supported",
                registered: true,
                scope: "workspace_files",
                reason: "content-addressed workspace snapshots are registered".to_owned(),
                dependency_detected: None,
            },
            BackendCard {
                id: "process-checkpoint",
                status: "unsupported",
                registered: false,
                scope: "process_memory",
                reason: "no process-memory checkpoint backend is registered in this build"
                    .to_owned(),
                dependency_detected: None,
            },
            BackendCard {
                id: "criu-checkpoint",
                status: "unsupported",
                registered: false,
                scope: "process_tree",
                reason: "CRIU tool presence does not imply a registered compatible backend"
                    .to_owned(),
                dependency_detected: Some(binary_in_path("criu")),
            },
        ],
    }
}

fn binary_in_path(name: &str) -> bool {
    env::var_os("PATH").is_some_and(|paths| {
        env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .any(|candidate| candidate.is_file())
    })
}
