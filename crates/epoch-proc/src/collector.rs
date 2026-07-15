use std::{
    collections::{BTreeSet, VecDeque},
    fs::{self, File},
    io::{self, Read},
    path::PathBuf,
};

use sha2::{Digest, Sha256};

use crate::{
    CollectionDiagnostic, CollectionIssueKind, CollectionSection, CollectorLimits, CommandIdentity,
    EncodedValue, ExecutableIdentity, FdKind, FdSummary, NamespaceIdentity, NetworkEndpoint,
    PROC_MANIFEST_SCHEMA_VERSION, ParseIssue, ProcCollection, ProcManifest, ProcessSnapshot,
    ThreadSnapshot, TransportProtocol, hex_bytes, normalize_fd_target, parse_cgroups,
    parse_inet_table, parse_maps, parse_namespace_target, parse_status, summarize_fd_targets,
};

#[derive(Clone, Debug)]
pub struct ProcCollector {
    root: PathBuf,
    limits: CollectorLimits,
}

impl ProcCollector {
    #[must_use]
    pub fn from_root(root: impl Into<PathBuf>, limits: CollectorLimits) -> Self {
        Self {
            root: root.into(),
            limits,
        }
    }

    #[must_use]
    pub fn collect(&self, root_pid: u32) -> ProcManifest {
        Session::new(self).run(root_pid)
    }
}

#[cfg(target_os = "linux")]
#[must_use]
pub fn collect_live(root_pid: u32, limits: CollectorLimits) -> ProcCollection {
    ProcCollection::Collected(ProcCollector::from_root("/proc", limits).collect(root_pid))
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn collect_live(_root_pid: u32, _limits: CollectorLimits) -> ProcCollection {
    ProcCollection::Unsupported(crate::UnsupportedCollection {
        platform: std::env::consts::OS.to_owned(),
        reason: "live process collection requires Linux procfs".to_owned(),
        metadata: [("required_os".to_owned(), "Linux".to_owned())]
            .into_iter()
            .collect(),
    })
}

struct Session<'a> {
    collector: &'a ProcCollector,
    diagnostics: Vec<CollectionDiagnostic>,
    truncated: bool,
}

struct ProcessBuild {
    snapshot: ProcessSnapshot,
    children: Vec<u32>,
}

impl<'a> Session<'a> {
    fn new(collector: &'a ProcCollector) -> Self {
        Self {
            collector,
            diagnostics: Vec::new(),
            truncated: false,
        }
    }

    fn run(mut self, root_pid: u32) -> ProcManifest {
        let mut queue = VecDeque::from([root_pid]);
        let mut seen = BTreeSet::new();
        let mut processes = Vec::new();
        while let Some(pid) = queue.pop_front() {
            if !seen.insert(pid) {
                continue;
            }
            if processes.len() >= self.collector.limits.max_processes {
                self.limit(
                    None,
                    None,
                    CollectionSection::Discovery,
                    "process_tree",
                    "process count reached max_processes",
                );
                break;
            }
            if let Some(process) = self.collect_process(pid) {
                queue.extend(process.children);
                processes.push(process.snapshot);
            }
        }
        processes.sort_by_key(|process| process.pid);
        ProcManifest {
            schema_version: PROC_MANIFEST_SCHEMA_VERSION,
            root_pid,
            processes,
            diagnostics: self.diagnostics,
            truncated: self.truncated,
        }
    }

    fn collect_process(&mut self, pid: u32) -> Option<ProcessBuild> {
        let status_resource = format!("{pid}/status");
        let status = match self.read_file(pid, None, CollectionSection::Status, &status_resource) {
            Ok(bytes) => {
                let parsed = parse_status(&bytes);
                self.parse_issues(
                    pid,
                    None,
                    CollectionSection::Status,
                    &status_resource,
                    parsed.issues,
                );
                Some(parsed.value)
            }
            Err(CollectionIssueKind::ProcessDisappeared) => return None,
            Err(_) => None,
        };
        let parent_pid = status.as_ref().and_then(|value| value.parent_pid);
        let threads = self.collect_threads(pid);
        let children = self.collect_children(pid, &threads);
        let maps = self.collect_maps(pid);
        let fds = self.collect_fds(pid);
        let namespaces = self.collect_namespaces(pid);
        let cgroups = self.collect_cgroups(pid);
        let network_endpoints = self.collect_network(pid, fds.as_ref());
        let executable = self.collect_executable(pid);
        let command = self.collect_command(pid);
        Some(ProcessBuild {
            snapshot: ProcessSnapshot {
                pid,
                parent_pid,
                status,
                threads,
                maps,
                fds,
                namespaces,
                cgroups,
                network_endpoints,
                executable,
                command,
            },
            children,
        })
    }

    fn collect_threads(&mut self, pid: u32) -> Vec<ThreadSnapshot> {
        let resource = format!("{pid}/task");
        let tids = self.numeric_directory(
            pid,
            CollectionSection::Threads,
            &resource,
            self.collector.limits.max_threads_per_process,
        );
        tids.into_iter()
            .filter_map(|tid| {
                let status_resource = format!("{pid}/task/{tid}/status");
                let bytes = self
                    .read_file(pid, Some(tid), CollectionSection::Threads, &status_resource)
                    .ok()?;
                let parsed = parse_status(&bytes);
                self.parse_issues(
                    pid,
                    Some(tid),
                    CollectionSection::Threads,
                    &status_resource,
                    parsed.issues,
                );
                Some(ThreadSnapshot {
                    tid,
                    name: parsed.value.name,
                    state: parsed.value.state,
                })
            })
            .collect()
    }

    fn collect_children(&mut self, pid: u32, threads: &[ThreadSnapshot]) -> Vec<u32> {
        let mut children = BTreeSet::new();
        for thread in threads {
            let resource = format!("{pid}/task/{}/children", thread.tid);
            let Ok(bytes) = self.read_file(
                pid,
                Some(thread.tid),
                CollectionSection::Discovery,
                &resource,
            ) else {
                continue;
            };
            for raw in String::from_utf8_lossy(&bytes).split_whitespace() {
                match raw.parse() {
                    Ok(child) => {
                        children.insert(child);
                    }
                    Err(_) => self.diagnostic(
                        Some(pid),
                        Some(thread.tid),
                        CollectionSection::Discovery,
                        CollectionIssueKind::MalformedEntry,
                        &resource,
                        "children file contains a non-numeric PID",
                    ),
                }
            }
        }
        children.into_iter().collect()
    }

    fn collect_maps(&mut self, pid: u32) -> Option<crate::MapsSummary> {
        let resource = format!("{pid}/maps");
        let bytes = self
            .read_file(pid, None, CollectionSection::Maps, &resource)
            .ok()?;
        let parsed = parse_maps(&bytes);
        self.parse_issues(pid, None, CollectionSection::Maps, &resource, parsed.issues);
        Some(parsed.value)
    }

    fn collect_fds(&mut self, pid: u32) -> Option<FdSummary> {
        let resource = format!("{pid}/fd");
        let fds = self.numeric_directory(
            pid,
            CollectionSection::Fds,
            &resource,
            self.collector.limits.max_fds_per_process,
        );
        if fds.is_empty() && !self.collector.root.join(&resource).is_dir() {
            return None;
        }
        let mut targets = Vec::new();
        for fd in fds {
            let link_resource = format!("{resource}/{fd}");
            let Ok(target) = self.read_link(pid, None, CollectionSection::Fds, &link_resource)
            else {
                continue;
            };
            let parsed = normalize_fd_target(target.as_os_str().as_encoded_bytes());
            self.parse_issues(
                pid,
                None,
                CollectionSection::Fds,
                &link_resource,
                parsed.issues,
            );
            targets.push(parsed.value);
        }
        Some(summarize_fd_targets(&targets))
    }

    fn collect_namespaces(&mut self, pid: u32) -> Vec<NamespaceIdentity> {
        const ALLOWED: &[&str] = &[
            "cgroup",
            "ipc",
            "mnt",
            "net",
            "pid",
            "pid_for_children",
            "time",
            "time_for_children",
            "user",
            "uts",
        ];
        let resource = format!("{pid}/ns");
        let entries = match fs::read_dir(self.collector.root.join(&resource)) {
            Ok(entries) => entries,
            Err(error) => {
                self.io_diagnostic(pid, None, CollectionSection::Namespaces, &resource, &error);
                return Vec::new();
            }
        };
        let mut names = BTreeSet::new();
        for entry in entries {
            let Ok(entry) = entry else {
                self.diagnostic(
                    Some(pid),
                    None,
                    CollectionSection::Namespaces,
                    CollectionIssueKind::Io,
                    &resource,
                    "failed to enumerate a namespace entry",
                );
                continue;
            };
            if let Some(name) = entry
                .file_name()
                .to_str()
                .filter(|name| ALLOWED.contains(name))
            {
                names.insert(name.to_owned());
            }
        }
        names
            .into_iter()
            .filter_map(|name| {
                let link_resource = format!("{resource}/{name}");
                let target = self
                    .read_link(pid, None, CollectionSection::Namespaces, &link_resource)
                    .ok()?;
                let parsed = parse_namespace_target(&name, target.as_os_str().as_encoded_bytes());
                self.parse_issues(
                    pid,
                    None,
                    CollectionSection::Namespaces,
                    &link_resource,
                    parsed.issues,
                );
                parsed.value
            })
            .collect()
    }

    fn collect_cgroups(&mut self, pid: u32) -> Vec<crate::CgroupMembership> {
        let resource = format!("{pid}/cgroup");
        let Ok(bytes) = self.read_file(pid, None, CollectionSection::Cgroup, &resource) else {
            return Vec::new();
        };
        let parsed = parse_cgroups(&bytes);
        self.parse_issues(
            pid,
            None,
            CollectionSection::Cgroup,
            &resource,
            parsed.issues,
        );
        parsed.value
    }

    fn collect_network(&mut self, pid: u32, fds: Option<&FdSummary>) -> Vec<NetworkEndpoint> {
        let socket_inodes: BTreeSet<_> = fds
            .into_iter()
            .flat_map(|summary| &summary.groups)
            .filter(|group| group.kind == FdKind::Socket)
            .filter_map(|group| group.object_id)
            .collect();
        if socket_inodes.is_empty() {
            return Vec::new();
        }
        let tables = [
            ("tcp", TransportProtocol::Tcp),
            ("tcp6", TransportProtocol::Tcp),
            ("udp", TransportProtocol::Udp),
            ("udp6", TransportProtocol::Udp),
        ];
        let mut endpoints = BTreeSet::new();
        for (name, protocol) in tables {
            let resource = format!("{pid}/net/{name}");
            let Ok(bytes) = self.read_file(pid, None, CollectionSection::Network, &resource) else {
                continue;
            };
            let parsed = parse_inet_table(&bytes, protocol);
            self.parse_issues(
                pid,
                None,
                CollectionSection::Network,
                &resource,
                parsed.issues,
            );
            endpoints.extend(
                parsed
                    .value
                    .into_iter()
                    .filter(|endpoint| socket_inodes.contains(&endpoint.inode)),
            );
        }
        let limit = self.collector.limits.max_network_entries;
        if endpoints.len() > limit {
            self.limit(
                Some(pid),
                None,
                CollectionSection::Network,
                &format!("{pid}/net"),
                "endpoint count reached max_network_entries",
            );
        }
        endpoints.into_iter().take(limit).collect()
    }

    fn collect_executable(&mut self, pid: u32) -> Option<ExecutableIdentity> {
        let resource = format!("{pid}/exe");
        let target = self
            .read_link(pid, None, CollectionSection::Executable, &resource)
            .ok()?;
        let target = EncodedValue::from_bytes(target.as_os_str().as_encoded_bytes());
        if target.raw_hex.is_some() {
            self.diagnostic(
                Some(pid),
                None,
                CollectionSection::Executable,
                CollectionIssueKind::Parse,
                &resource,
                "executable link is not UTF-8; exact bytes retained as hex",
            );
        }
        Some(ExecutableIdentity { target })
    }

    fn collect_command(&mut self, pid: u32) -> Option<CommandIdentity> {
        let resource = format!("{pid}/cmdline");
        let bytes = self
            .read_file(pid, None, CollectionSection::Command, &resource)
            .ok()?;
        let argument_count = bytes
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .count();
        Some(CommandIdentity {
            argument_count,
            sha256: hex_bytes(&Sha256::digest(&bytes)),
        })
    }

    fn numeric_directory(
        &mut self,
        pid: u32,
        section: CollectionSection,
        resource: &str,
        limit: usize,
    ) -> Vec<u32> {
        let entries = match fs::read_dir(self.collector.root.join(resource)) {
            Ok(entries) => entries,
            Err(error) => {
                self.io_diagnostic(pid, None, section, resource, &error);
                return Vec::new();
            }
        };
        let mut values = Vec::new();
        for entry in entries.take(limit.saturating_add(1)) {
            let Ok(entry) = entry else {
                self.diagnostic(
                    Some(pid),
                    None,
                    section,
                    CollectionIssueKind::Io,
                    resource,
                    "failed to enumerate a directory entry",
                );
                continue;
            };
            let Some(value) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                self.diagnostic(
                    Some(pid),
                    None,
                    section,
                    CollectionIssueKind::MalformedEntry,
                    resource,
                    "ignored a non-numeric procfs directory entry",
                );
                continue;
            };
            values.push(value);
        }
        if values.len() > limit {
            self.limit(
                Some(pid),
                None,
                section,
                resource,
                "directory entry count reached its configured limit",
            );
            values.truncate(limit);
        }
        values.sort_unstable();
        values
    }

    fn read_file(
        &mut self,
        pid: u32,
        tid: Option<u32>,
        section: CollectionSection,
        resource: &str,
    ) -> Result<Vec<u8>, CollectionIssueKind> {
        let path = self.collector.root.join(resource);
        let mut file = File::open(path)
            .map_err(|error| self.io_diagnostic(pid, tid, section, resource, &error))?;
        let limit = self.collector.limits.max_file_bytes;
        let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
        let mut bytes = Vec::new();
        file.by_ref()
            .take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(|error| self.io_diagnostic(pid, tid, section, resource, &error))?;
        if bytes.len() > limit {
            self.limit(
                Some(pid),
                tid,
                section,
                resource,
                "file exceeded max_file_bytes",
            );
            return Err(CollectionIssueKind::LimitExceeded);
        }
        Ok(bytes)
    }

    fn read_link(
        &mut self,
        pid: u32,
        tid: Option<u32>,
        section: CollectionSection,
        resource: &str,
    ) -> Result<PathBuf, CollectionIssueKind> {
        fs::read_link(self.collector.root.join(resource))
            .map_err(|error| self.io_diagnostic(pid, tid, section, resource, &error))
    }

    fn parse_issues(
        &mut self,
        pid: u32,
        tid: Option<u32>,
        section: CollectionSection,
        resource: &str,
        issues: Vec<ParseIssue>,
    ) {
        for issue in issues {
            self.diagnostic(
                Some(pid),
                tid,
                section,
                CollectionIssueKind::Parse,
                resource,
                &format!("{:?}: {}", issue.kind, issue.detail),
            );
        }
    }

    fn io_diagnostic(
        &mut self,
        pid: u32,
        tid: Option<u32>,
        section: CollectionSection,
        resource: &str,
        error: &io::Error,
    ) -> CollectionIssueKind {
        let kind = classify_io_error(error);
        self.diagnostic(Some(pid), tid, section, kind, resource, &error.to_string());
        kind
    }

    fn limit(
        &mut self,
        pid: Option<u32>,
        tid: Option<u32>,
        section: CollectionSection,
        resource: &str,
        detail: &str,
    ) {
        self.truncated = true;
        self.diagnostic(
            pid,
            tid,
            section,
            CollectionIssueKind::LimitExceeded,
            resource,
            detail,
        );
    }

    fn diagnostic(
        &mut self,
        pid: Option<u32>,
        tid: Option<u32>,
        section: CollectionSection,
        kind: CollectionIssueKind,
        resource: &str,
        detail: &str,
    ) {
        self.diagnostics.push(CollectionDiagnostic {
            pid,
            tid,
            section,
            kind,
            resource: resource.to_owned(),
            detail: detail.to_owned(),
        });
    }
}

fn classify_io_error(error: &io::Error) -> CollectionIssueKind {
    match error.kind() {
        io::ErrorKind::NotFound => CollectionIssueKind::ProcessDisappeared,
        io::ErrorKind::PermissionDenied => CollectionIssueKind::PermissionDenied,
        _ => CollectionIssueKind::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_denied_is_a_distinct_diagnostic_kind() {
        let error = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(
            classify_io_error(&error),
            CollectionIssueKind::PermissionDenied
        );
    }

    #[test]
    fn not_found_is_treated_as_a_process_race() {
        let error = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(
            classify_io_error(&error),
            CollectionIssueKind::ProcessDisappeared
        );
    }
}
