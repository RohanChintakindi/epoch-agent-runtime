use std::collections::HashSet;

use crate::{
    CapabilityName, CapabilitySet, CgroupMembership, IdQuad, MapsSummary, ParseIssue,
    ParseIssueKind, Parsed, ProcessState, ProcessStatus,
};

#[must_use]
pub fn parse_status(input: &[u8]) -> Parsed<ProcessStatus> {
    let (text, mut issues) = input_text(input);
    let mut value = ProcessStatus::default();
    let mut seen = HashSet::new();

    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let Some((field, raw)) = line.split_once(':') else {
            issues.push(issue(
                ParseIssueKind::MalformedLine,
                line_number,
                None,
                "status line has no colon",
            ));
            continue;
        };
        if !seen.insert(field.to_owned()) {
            issues.push(issue(
                ParseIssueKind::DuplicateField,
                line_number,
                Some(field),
                "duplicate status field",
            ));
            continue;
        }
        let raw = raw.trim();
        match field {
            "Name" => value.name = Some(raw.to_owned()),
            "State" => value.state = parse_state(raw, field, line_number, &mut issues),
            "Tgid" => value.tgid = parse_number(raw, field, line_number, &mut issues),
            "Pid" => value.pid = parse_number(raw, field, line_number, &mut issues),
            "PPid" => value.parent_pid = parse_number(raw, field, line_number, &mut issues),
            "TracerPid" => value.tracer_pid = parse_number(raw, field, line_number, &mut issues),
            "Umask" => value.umask = parse_octal(raw, field, line_number, &mut issues),
            "Uid" => value.user_ids = parse_id_quad(raw, field, line_number, &mut issues),
            "Gid" => value.group_ids = parse_id_quad(raw, field, line_number, &mut issues),
            "NSpid" => {
                value.namespace_pids = parse_number_list(raw, field, line_number, &mut issues);
            }
            "Threads" => value.thread_count = parse_positive(raw, field, line_number, &mut issues),
            "VmSize" => value.memory.vm_size_kib = parse_kib(raw, field, line_number, &mut issues),
            "VmRSS" => value.memory.rss_kib = parse_kib(raw, field, line_number, &mut issues),
            "RssAnon" => {
                value.memory.rss_anon_kib = parse_kib(raw, field, line_number, &mut issues);
            }
            "RssFile" => {
                value.memory.rss_file_kib = parse_kib(raw, field, line_number, &mut issues);
            }
            "RssShmem" => {
                value.memory.rss_shmem_kib = parse_kib(raw, field, line_number, &mut issues);
            }
            "VmData" => value.memory.data_kib = parse_kib(raw, field, line_number, &mut issues),
            "VmStk" => value.memory.stack_kib = parse_kib(raw, field, line_number, &mut issues),
            "VmExe" => {
                value.memory.executable_kib = parse_kib(raw, field, line_number, &mut issues);
            }
            "VmLib" => value.memory.libraries_kib = parse_kib(raw, field, line_number, &mut issues),
            "VmPTE" => {
                value.memory.page_tables_kib = parse_kib(raw, field, line_number, &mut issues);
            }
            "VmSwap" => value.memory.swap_kib = parse_kib(raw, field, line_number, &mut issues),
            "CapInh" => {
                value.capabilities.inheritable =
                    parse_capability(raw, field, line_number, &mut issues);
            }
            "CapPrm" => {
                value.capabilities.permitted =
                    parse_capability(raw, field, line_number, &mut issues);
            }
            "CapEff" => {
                value.capabilities.effective =
                    parse_capability(raw, field, line_number, &mut issues);
            }
            "CapBnd" => {
                value.capabilities.bounding =
                    parse_capability(raw, field, line_number, &mut issues);
            }
            "CapAmb" => {
                value.capabilities.ambient = parse_capability(raw, field, line_number, &mut issues);
            }
            "NoNewPrivs" => {
                value.no_new_privileges = parse_bool(raw, field, line_number, &mut issues);
            }
            "Seccomp" => value.seccomp_mode = parse_number(raw, field, line_number, &mut issues),
            "Seccomp_filters" => {
                value.seccomp_filters = parse_number(raw, field, line_number, &mut issues);
            }
            "voluntary_ctxt_switches" => {
                value.voluntary_context_switches =
                    parse_number(raw, field, line_number, &mut issues);
            }
            "nonvoluntary_ctxt_switches" => {
                value.nonvoluntary_context_switches =
                    parse_number(raw, field, line_number, &mut issues);
            }
            _ => {}
        }
    }
    Parsed { value, issues }
}

#[must_use]
pub fn parse_maps(input: &[u8]) -> Parsed<MapsSummary> {
    let (text, mut issues) = input_text(input);
    let mut value = MapsSummary::default();

    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let Some((bytes, permissions, path)) = parse_map_line(line, line_number, &mut issues)
        else {
            continue;
        };
        value.region_count += 1;
        add_bytes(&mut value.mapped_bytes, bytes, line_number, &mut issues);
        if permissions[2] == b'x' {
            add_bytes(&mut value.executable_bytes, bytes, line_number, &mut issues);
        }
        if permissions[1] == b'w' && permissions[3] == b'p' {
            add_bytes(
                &mut value.writable_private_bytes,
                bytes,
                line_number,
                &mut issues,
            );
        }
        if permissions[3] == b's' {
            add_bytes(&mut value.shared_bytes, bytes, line_number, &mut issues);
        }
        match path.as_deref() {
            Some(path) if path.starts_with('/') => {
                add_bytes(
                    &mut value.file_backed_bytes,
                    bytes,
                    line_number,
                    &mut issues,
                );
                if path.ends_with(" (deleted)") {
                    value.deleted_file_regions += 1;
                }
            }
            Some(path) if path.starts_with('[') => {
                add_bytes(&mut value.special_bytes, bytes, line_number, &mut issues);
            }
            None | Some(_) => {
                add_bytes(&mut value.anonymous_bytes, bytes, line_number, &mut issues);
            }
        }
    }
    Parsed { value, issues }
}

#[must_use]
pub fn parse_cgroups(input: &[u8]) -> Parsed<Vec<CgroupMembership>> {
    let (text, mut issues) = input_text(input);
    let mut value = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let columns: Vec<_> = line.splitn(3, ':').collect();
        if columns.len() != 3 || columns[2].is_empty() || !columns[2].starts_with('/') {
            issues.push(issue(
                ParseIssueKind::MalformedLine,
                line_number,
                None,
                "invalid cgroup membership line",
            ));
            continue;
        }
        let Ok(hierarchy_id) = columns[0].parse() else {
            issues.push(issue(
                ParseIssueKind::InvalidValue,
                line_number,
                Some("hierarchy_id"),
                "invalid cgroup hierarchy id",
            ));
            continue;
        };
        let controllers = if columns[1].is_empty() {
            Vec::new()
        } else {
            columns[1].split(',').map(str::to_owned).collect()
        };
        value.push(CgroupMembership {
            hierarchy_id,
            controllers,
            path: columns[2].to_owned(),
        });
    }
    Parsed { value, issues }
}

#[must_use]
pub fn decode_capability_mask(mask: u64) -> CapabilitySet {
    let mut names = Vec::new();
    let mut unknown_bits = Vec::new();
    for bit in 0..64 {
        if mask & (1_u64 << bit) == 0 {
            continue;
        }
        match capability_name(bit) {
            Some(name) => names.push(name),
            None => unknown_bits.push(bit),
        }
    }
    CapabilitySet {
        raw_hex: format!("{mask:016x}"),
        names,
        unknown_bits,
    }
}

fn capability_name(bit: u8) -> Option<CapabilityName> {
    const NAMES: [CapabilityName; 41] = [
        CapabilityName::Chown,
        CapabilityName::DacOverride,
        CapabilityName::DacReadSearch,
        CapabilityName::Fowner,
        CapabilityName::Fsetid,
        CapabilityName::Kill,
        CapabilityName::Setgid,
        CapabilityName::Setuid,
        CapabilityName::Setpcap,
        CapabilityName::LinuxImmutable,
        CapabilityName::NetBindService,
        CapabilityName::NetBroadcast,
        CapabilityName::NetAdmin,
        CapabilityName::NetRaw,
        CapabilityName::IpcLock,
        CapabilityName::IpcOwner,
        CapabilityName::SysModule,
        CapabilityName::SysRawio,
        CapabilityName::SysChroot,
        CapabilityName::SysPtrace,
        CapabilityName::SysPacct,
        CapabilityName::SysAdmin,
        CapabilityName::SysBoot,
        CapabilityName::SysNice,
        CapabilityName::SysResource,
        CapabilityName::SysTime,
        CapabilityName::SysTtyConfig,
        CapabilityName::Mknod,
        CapabilityName::Lease,
        CapabilityName::AuditWrite,
        CapabilityName::AuditControl,
        CapabilityName::Setfcap,
        CapabilityName::MacOverride,
        CapabilityName::MacAdmin,
        CapabilityName::Syslog,
        CapabilityName::WakeAlarm,
        CapabilityName::BlockSuspend,
        CapabilityName::AuditRead,
        CapabilityName::Perfmon,
        CapabilityName::Bpf,
        CapabilityName::CheckpointRestore,
    ];
    NAMES.get(usize::from(bit)).copied()
}

fn parse_map_line(
    line: &str,
    line_number: usize,
    issues: &mut Vec<ParseIssue>,
) -> Option<(u64, [u8; 4], Option<String>)> {
    let columns: Vec<_> = line.split_whitespace().collect();
    if columns.len() < 5 {
        issues.push(issue(
            ParseIssueKind::MalformedLine,
            line_number,
            None,
            "maps line has fewer than five columns",
        ));
        return None;
    }
    let Some((start, end)) = columns[0].split_once('-') else {
        issues.push(issue(
            ParseIssueKind::MalformedLine,
            line_number,
            Some("address"),
            "maps address is not a range",
        ));
        return None;
    };
    let start = parse_hex_address(start, "start", line_number, issues)?;
    let end = parse_hex_address(end, "end", line_number, issues)?;
    let Some(bytes) = end.checked_sub(start) else {
        invalid("address", line_number, "end address precedes start", issues);
        return None;
    };
    let Ok(permissions) = <[u8; 4]>::try_from(columns[1].as_bytes()) else {
        invalid(
            "permissions",
            line_number,
            "invalid maps permissions",
            issues,
        );
        return None;
    };
    if !matches!(permissions[3], b'p' | b's') {
        invalid(
            "permissions",
            line_number,
            "invalid maps permissions",
            issues,
        );
        return None;
    }
    let path = (columns.len() > 5).then(|| columns[5..].join(" "));
    Some((bytes, permissions, path))
}

fn parse_hex_address(
    raw: &str,
    name: &str,
    line: usize,
    issues: &mut Vec<ParseIssue>,
) -> Option<u64> {
    u64::from_str_radix(raw, 16)
        .map_err(|_| invalid("address", line, &format!("invalid {name} address"), issues))
        .ok()
}

fn input_text(input: &[u8]) -> (String, Vec<ParseIssue>) {
    match std::str::from_utf8(input) {
        Ok(text) => (text.to_owned(), Vec::new()),
        Err(error) => (
            String::from_utf8_lossy(input).into_owned(),
            vec![ParseIssue {
                kind: ParseIssueKind::NonUtf8,
                line: None,
                field: None,
                detail: format!("invalid UTF-8 at byte {}", error.valid_up_to()),
            }],
        ),
    }
}

fn issue(
    kind: ParseIssueKind,
    line: usize,
    field: Option<&str>,
    detail: impl Into<String>,
) -> ParseIssue {
    ParseIssue {
        kind,
        line: Some(line),
        field: field.map(str::to_owned),
        detail: detail.into(),
    }
}

fn invalid(field: &str, line: usize, detail: &str, issues: &mut Vec<ParseIssue>) {
    issues.push(issue(
        ParseIssueKind::InvalidValue,
        line,
        Some(field),
        detail,
    ));
}

fn parse_number<T: std::str::FromStr>(
    raw: &str,
    field: &str,
    line: usize,
    issues: &mut Vec<ParseIssue>,
) -> Option<T> {
    raw.parse()
        .map_err(|_| invalid(field, line, "invalid decimal integer", issues))
        .ok()
}

fn parse_positive(
    raw: &str,
    field: &str,
    line: usize,
    issues: &mut Vec<ParseIssue>,
) -> Option<u32> {
    let value = parse_number(raw, field, line, issues)?;
    if value == 0 {
        invalid(field, line, "value must be positive", issues);
        return None;
    }
    Some(value)
}

fn parse_octal(raw: &str, field: &str, line: usize, issues: &mut Vec<ParseIssue>) -> Option<u32> {
    u32::from_str_radix(raw, 8)
        .map_err(|_| invalid(field, line, "invalid octal integer", issues))
        .ok()
}

fn parse_state(
    raw: &str,
    field: &str,
    line: usize,
    issues: &mut Vec<ParseIssue>,
) -> Option<ProcessState> {
    let mut chars = raw.chars();
    let Some(code) = chars.next() else {
        invalid(field, line, "missing process state", issues);
        return None;
    };
    if !code.is_ascii_alphabetic() {
        invalid(field, line, "invalid process state code", issues);
        return None;
    }
    let description = chars.as_str().trim();
    Some(ProcessState {
        code,
        description: (!description.is_empty()).then(|| description.to_owned()),
    })
}

fn parse_id_quad(
    raw: &str,
    field: &str,
    line: usize,
    issues: &mut Vec<ParseIssue>,
) -> Option<IdQuad> {
    let values: Vec<_> = raw.split_whitespace().collect();
    if values.len() != 4 {
        invalid(field, line, "expected four identifiers", issues);
        return None;
    }
    let parsed: Option<Vec<u32>> = values.iter().map(|value| value.parse().ok()).collect();
    let Some(parsed) = parsed else {
        invalid(field, line, "invalid identifier", issues);
        return None;
    };
    Some(IdQuad {
        real: parsed[0],
        effective: parsed[1],
        saved: parsed[2],
        filesystem: parsed[3],
    })
}

fn parse_number_list(
    raw: &str,
    field: &str,
    line: usize,
    issues: &mut Vec<ParseIssue>,
) -> Vec<u32> {
    let values: Option<Vec<u32>> = raw
        .split_whitespace()
        .map(|value| value.parse().ok())
        .collect();
    match values {
        Some(values) if !values.is_empty() => values,
        _ => {
            invalid(field, line, "invalid or empty integer list", issues);
            Vec::new()
        }
    }
}

fn parse_kib(raw: &str, field: &str, line: usize, issues: &mut Vec<ParseIssue>) -> Option<u64> {
    let columns: Vec<_> = raw.split_whitespace().collect();
    if columns.len() != 2 || columns[1] != "kB" {
        invalid(field, line, "expected an integer followed by kB", issues);
        return None;
    }
    parse_number(columns[0], field, line, issues)
}

fn parse_capability(
    raw: &str,
    field: &str,
    line: usize,
    issues: &mut Vec<ParseIssue>,
) -> Option<CapabilitySet> {
    u64::from_str_radix(raw, 16)
        .map(decode_capability_mask)
        .map_err(|_| invalid(field, line, "invalid hexadecimal capability mask", issues))
        .ok()
}

fn parse_bool(raw: &str, field: &str, line: usize, issues: &mut Vec<ParseIssue>) -> Option<bool> {
    match raw {
        "0" => Some(false),
        "1" => Some(true),
        _ => {
            invalid(field, line, "expected 0 or 1", issues);
            None
        }
    }
}

fn add_bytes(target: &mut u64, bytes: u64, line: usize, issues: &mut Vec<ParseIssue>) {
    match target.checked_add(bytes) {
        Some(total) => *target = total,
        None => issues.push(issue(
            ParseIssueKind::Overflow,
            line,
            None,
            "maps byte total overflowed",
        )),
    }
}
