#![cfg(unix)]

use std::{fs, os::unix::fs::symlink, path::Path};

use epoch_proc::{
    CollectionIssueKind, CollectionSection, CollectorLimits, PROC_MANIFEST_SCHEMA_VERSION,
    ProcCollection, ProcCollector, collect_live,
};
use tempfile::TempDir;

const ROOT_STATUS: &str = "Name:\troot-agent\nState:\tS (sleeping)\nTgid:\t100\nPid:\t100\nPPid:\t1\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\nThreads:\t2\nCapEff:\t0000000000000401\n";
const CHILD_STATUS: &str = "Name:\tworker\nState:\tR (running)\nTgid:\t101\nPid:\t101\nPPid:\t100\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\nThreads:\t1\n";
const THREAD_STATUS: &str = "Name:\tio-thread\nState:\tS (sleeping)\nTgid:\t100\nPid:\t102\nPPid:\t1\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\t1000\t1000\nThreads:\t2\n";
const MAPS: &str = "00400000-00401000 r-xp 00000000 08:02 1 /usr/bin/root-agent\n";
const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345\n   1: 0100007F:2328 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 99999\n";

#[test]
fn collects_versioned_process_tree_and_partial_semantic_state() {
    let fixture = fixture_proc();
    let collector = ProcCollector::from_root(fixture.path(), CollectorLimits::default());
    let manifest = collector.collect(100);

    assert_eq!(manifest.schema_version, PROC_MANIFEST_SCHEMA_VERSION);
    assert_eq!(manifest.root_pid, 100);
    assert!(!manifest.truncated);
    assert_eq!(manifest.processes.len(), 2);

    let root = manifest
        .processes
        .iter()
        .find(|item| item.pid == 100)
        .expect("root");
    assert_eq!(root.parent_pid, Some(1));
    assert_eq!(
        root.status.as_ref().expect("status").name.as_deref(),
        Some("root-agent")
    );
    assert_eq!(root.threads.len(), 2);
    assert_eq!(root.threads[1].tid, 102);
    assert_eq!(root.maps.as_ref().expect("maps").mapped_bytes, 4096);
    assert_eq!(root.fds.as_ref().expect("fds").total, 3);
    assert_eq!(root.namespaces.len(), 2);
    assert_eq!(root.cgroups.len(), 1);
    assert_eq!(root.network_endpoints.len(), 1);
    assert_eq!(root.network_endpoints[0].inode, 12345);
    assert_eq!(
        root.executable.as_ref().expect("exe").target.display,
        "/usr/bin/root-agent"
    );
    assert_eq!(root.command.as_ref().expect("command").argument_count, 2);
    assert_eq!(root.command.as_ref().expect("command").sha256.len(), 64);

    let child = manifest
        .processes
        .iter()
        .find(|item| item.pid == 101)
        .expect("child");
    assert_eq!(child.parent_pid, Some(100));
    assert!(child.maps.is_none());
    assert!(manifest.diagnostics.iter().any(|diagnostic| {
        diagnostic.pid == Some(101)
            && diagnostic.section == CollectionSection::Maps
            && diagnostic.kind == CollectionIssueKind::ProcessDisappeared
    }));
    assert!(manifest.diagnostics.iter().any(|diagnostic| {
        diagnostic.pid == Some(101)
            && diagnostic.section == CollectionSection::Cgroup
            && diagnostic.kind == CollectionIssueKind::Parse
    }));
}

#[test]
fn declared_child_that_disappears_is_a_diagnostic_not_a_failure() {
    let fixture = fixture_proc();
    fs::write(fixture.path().join("100/task/100/children"), "101 999\n").expect("children");
    let manifest =
        ProcCollector::from_root(fixture.path(), CollectorLimits::default()).collect(100);

    assert_eq!(manifest.processes.len(), 2);
    assert!(manifest.diagnostics.iter().any(|diagnostic| {
        diagnostic.pid == Some(999)
            && diagnostic.section == CollectionSection::Status
            && diagnostic.kind == CollectionIssueKind::ProcessDisappeared
    }));
}

#[test]
fn collection_limits_bound_reads_and_process_count() {
    let fixture = fixture_proc();
    let limits = CollectorLimits {
        max_processes: 1,
        max_threads_per_process: 1,
        max_fds_per_process: 2,
        max_file_bytes: 32,
        max_network_entries: 1,
    };
    let manifest = ProcCollector::from_root(fixture.path(), limits).collect(100);

    assert!(manifest.truncated);
    assert_eq!(manifest.processes.len(), 1);
    assert!(manifest.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == CollectionIssueKind::LimitExceeded
            && diagnostic.section == CollectionSection::Status
    }));
    assert!(manifest.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == CollectionIssueKind::LimitExceeded
            && diagnostic.section == CollectionSection::Fds
    }));
}

#[test]
fn manifest_json_carries_the_schema_version() {
    let fixture = fixture_proc();
    let manifest =
        ProcCollector::from_root(fixture.path(), CollectorLimits::default()).collect(100);
    let json = serde_json::to_value(manifest).expect("serialize manifest");

    assert_eq!(json["schema_version"], PROC_MANIFEST_SCHEMA_VERSION);
    assert_eq!(json["root_pid"], 100);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn live_collection_is_explicitly_unsupported_off_linux() {
    let result = collect_live(1, CollectorLimits::default());

    let ProcCollection::Unsupported(unsupported) = result else {
        panic!("expected unsupported result");
    };
    assert!(!unsupported.platform.is_empty());
    assert!(unsupported.reason.contains("Linux"));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_live_proc_collection_smoke_test() {
    let result = collect_live(std::process::id(), CollectorLimits::default());

    let ProcCollection::Collected(manifest) = result else {
        panic!("expected live manifest");
    };
    assert_eq!(manifest.root_pid, std::process::id());
    assert!(!manifest.processes.is_empty());
}

fn fixture_proc() -> TempDir {
    let fixture = TempDir::new().expect("temp proc root");
    create_process(fixture.path(), 100, ROOT_STATUS);
    create_process(fixture.path(), 101, CHILD_STATUS);

    write(fixture.path(), "100/maps", MAPS.as_bytes());
    write(fixture.path(), "100/cgroup", b"0::/agent.slice\n");
    write(fixture.path(), "100/cmdline", b"agent\0--serve\0");
    write(fixture.path(), "100/task/100/children", b"101\n");
    write(
        fixture.path(),
        "100/task/102/status",
        THREAD_STATUS.as_bytes(),
    );
    write(fixture.path(), "100/task/102/children", b"");
    write(fixture.path(), "100/net/tcp", TCP.as_bytes());
    write(
        fixture.path(),
        "100/net/tcp6",
        TCP.lines().next().expect("header").as_bytes(),
    );
    write(
        fixture.path(),
        "100/net/udp",
        TCP.lines().next().expect("header").as_bytes(),
    );
    write(
        fixture.path(),
        "100/net/udp6",
        TCP.lines().next().expect("header").as_bytes(),
    );

    make_link(fixture.path(), "/usr/bin/root-agent", "100/exe");
    make_link(fixture.path(), "socket:[12345]", "100/fd/3");
    make_link(fixture.path(), "/tmp/output", "100/fd/4");
    make_link(fixture.path(), "pipe:[88]", "100/fd/5");
    make_link(fixture.path(), "mnt:[4026531840]", "100/ns/mnt");
    make_link(fixture.path(), "net:[4026531999]", "100/ns/net");

    write(fixture.path(), "101/cgroup", b"malformed\n");
    write(fixture.path(), "101/task/101/children", b"");
    fixture
}

fn create_process(root: &Path, pid: u32, status: &str) {
    write(root, &format!("{pid}/status"), status.as_bytes());
    write(root, &format!("{pid}/task/{pid}/status"), status.as_bytes());
}

fn write(root: &Path, relative: &str, content: &[u8]) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    fs::write(path, content).expect("write fixture");
}

fn make_link(root: &Path, target: &str, relative: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
    symlink(target, path).expect("create symlink");
}
