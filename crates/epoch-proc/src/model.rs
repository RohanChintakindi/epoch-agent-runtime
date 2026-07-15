use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const PROC_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Parsed<T> {
    pub value: T,
    pub issues: Vec<ParseIssue>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParseIssue {
    pub kind: ParseIssueKind,
    pub line: Option<usize>,
    pub field: Option<String>,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseIssueKind {
    NonUtf8,
    MalformedLine,
    InvalidValue,
    DuplicateField,
    Overflow,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessStatus {
    pub name: Option<String>,
    pub state: Option<ProcessState>,
    pub tgid: Option<u32>,
    pub pid: Option<u32>,
    pub parent_pid: Option<u32>,
    pub tracer_pid: Option<u32>,
    pub umask: Option<u32>,
    pub user_ids: Option<IdQuad>,
    pub group_ids: Option<IdQuad>,
    pub namespace_pids: Vec<u32>,
    pub thread_count: Option<u32>,
    pub memory: StatusMemory,
    pub capabilities: CapabilityMasks,
    pub no_new_privileges: Option<bool>,
    pub seccomp_mode: Option<u32>,
    pub seccomp_filters: Option<u32>,
    pub voluntary_context_switches: Option<u64>,
    pub nonvoluntary_context_switches: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessState {
    pub code: char,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IdQuad {
    pub real: u32,
    pub effective: u32,
    pub saved: u32,
    pub filesystem: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct StatusMemory {
    pub vm_size_kib: Option<u64>,
    pub rss_kib: Option<u64>,
    pub rss_anon_kib: Option<u64>,
    pub rss_file_kib: Option<u64>,
    pub rss_shmem_kib: Option<u64>,
    pub data_kib: Option<u64>,
    pub stack_kib: Option<u64>,
    pub executable_kib: Option<u64>,
    pub libraries_kib: Option<u64>,
    pub page_tables_kib: Option<u64>,
    pub swap_kib: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityMasks {
    pub inheritable: Option<CapabilitySet>,
    pub permitted: Option<CapabilitySet>,
    pub effective: Option<CapabilitySet>,
    pub bounding: Option<CapabilitySet>,
    pub ambient: Option<CapabilitySet>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilitySet {
    pub raw_hex: String,
    pub names: Vec<CapabilityName>,
    pub unknown_bits: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CapabilityName {
    Chown,
    DacOverride,
    DacReadSearch,
    Fowner,
    Fsetid,
    Kill,
    Setgid,
    Setuid,
    Setpcap,
    LinuxImmutable,
    NetBindService,
    NetBroadcast,
    NetAdmin,
    NetRaw,
    IpcLock,
    IpcOwner,
    SysModule,
    SysRawio,
    SysChroot,
    SysPtrace,
    SysPacct,
    SysAdmin,
    SysBoot,
    SysNice,
    SysResource,
    SysTime,
    SysTtyConfig,
    Mknod,
    Lease,
    AuditWrite,
    AuditControl,
    Setfcap,
    MacOverride,
    MacAdmin,
    Syslog,
    WakeAlarm,
    BlockSuspend,
    AuditRead,
    Perfmon,
    Bpf,
    CheckpointRestore,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MapsSummary {
    pub region_count: u64,
    pub mapped_bytes: u64,
    pub executable_bytes: u64,
    pub writable_private_bytes: u64,
    pub file_backed_bytes: u64,
    pub anonymous_bytes: u64,
    pub special_bytes: u64,
    pub shared_bytes: u64,
    pub deleted_file_regions: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CgroupMembership {
    pub hierarchy_id: u32,
    pub controllers: Vec<String>,
    pub path: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct EncodedValue {
    pub display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_hex: Option<String>,
}

impl EncodedValue {
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        match std::str::from_utf8(bytes) {
            Ok(value) => Self {
                display: value.to_owned(),
                raw_hex: None,
            },
            Err(_) => Self {
                display: String::from_utf8_lossy(bytes).into_owned(),
                raw_hex: Some(hex_bytes(bytes)),
            },
        }
    }
}

pub(crate) fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(HEX[usize::from(byte >> 4)]));
        result.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    result
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FdKind {
    Path,
    Socket,
    Pipe,
    AnonInode,
    Memfd,
    Other,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NormalizedFdTarget {
    pub kind: FdKind,
    pub target: EncodedValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<u64>,
    pub deleted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FdSummary {
    pub total: u64,
    pub groups: Vec<FdGroup>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FdGroup {
    pub kind: FdKind,
    pub target: EncodedValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<u64>,
    pub deleted: bool,
    pub count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NamespaceIdentity {
    pub kind: String,
    pub inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportProtocol {
    Tcp,
    Udp,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRole {
    Listener,
    Connected,
    Unconnected,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketState {
    Established,
    SynSent,
    SynReceived,
    FinWait1,
    FinWait2,
    TimeWait,
    Closed,
    CloseWait,
    LastAck,
    Listen,
    Closing,
    NewSynReceived,
    Unknown(u8),
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NetworkAddress {
    pub address: String,
    pub port: u16,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct NetworkEndpoint {
    pub protocol: TransportProtocol,
    pub role: NetworkRole,
    pub state: SocketState,
    pub local: NetworkAddress,
    pub remote: NetworkAddress,
    pub inode: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CollectorLimits {
    pub max_processes: usize,
    pub max_threads_per_process: usize,
    pub max_fds_per_process: usize,
    pub max_file_bytes: usize,
    pub max_network_entries: usize,
}

impl Default for CollectorLimits {
    fn default() -> Self {
        Self {
            max_processes: 1_024,
            max_threads_per_process: 2_048,
            max_fds_per_process: 4_096,
            max_file_bytes: 8 * 1_024 * 1_024,
            max_network_entries: 8_192,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcManifest {
    pub schema_version: u32,
    pub root_pid: u32,
    pub processes: Vec<ProcessSnapshot>,
    pub diagnostics: Vec<CollectionDiagnostic>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub status: Option<ProcessStatus>,
    pub threads: Vec<ThreadSnapshot>,
    pub maps: Option<MapsSummary>,
    pub fds: Option<FdSummary>,
    pub namespaces: Vec<NamespaceIdentity>,
    pub cgroups: Vec<CgroupMembership>,
    pub network_endpoints: Vec<NetworkEndpoint>,
    pub executable: Option<ExecutableIdentity>,
    pub command: Option<CommandIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ThreadSnapshot {
    pub tid: u32,
    pub name: Option<String>,
    pub state: Option<ProcessState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutableIdentity {
    pub target: EncodedValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandIdentity {
    pub argument_count: usize,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionIssueKind {
    ProcessDisappeared,
    PermissionDenied,
    LimitExceeded,
    Io,
    Parse,
    MalformedEntry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionSection {
    Discovery,
    Status,
    Threads,
    Maps,
    Fds,
    Namespaces,
    Cgroup,
    Network,
    Executable,
    Command,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CollectionDiagnostic {
    pub pid: Option<u32>,
    pub tid: Option<u32>,
    pub section: CollectionSection,
    pub kind: CollectionIssueKind,
    pub resource: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "data", rename_all = "snake_case")]
pub enum ProcCollection {
    Collected(ProcManifest),
    Unsupported(UnsupportedCollection),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UnsupportedCollection {
    pub platform: String,
    pub reason: String,
    pub metadata: BTreeMap<String, String>,
}
