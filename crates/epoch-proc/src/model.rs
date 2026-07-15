use serde::{Deserialize, Serialize};

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
