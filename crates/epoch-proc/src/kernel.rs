use std::{collections::BTreeMap, net::Ipv6Addr};

use crate::{
    EncodedValue, FdGroup, FdKind, FdSummary, NamespaceIdentity, NetworkAddress, NetworkEndpoint,
    NetworkRole, NormalizedFdTarget, ParseIssue, ParseIssueKind, Parsed, SocketState,
    TransportProtocol,
};

#[must_use]
pub fn normalize_fd_target(bytes: &[u8]) -> Parsed<NormalizedFdTarget> {
    let target = EncodedValue::from_bytes(bytes);
    let mut issues = Vec::new();
    if target.raw_hex.is_some() {
        issues.push(ParseIssue {
            kind: ParseIssueKind::NonUtf8,
            line: None,
            field: Some("fd_target".to_owned()),
            detail: "FD link target is not UTF-8; exact bytes retained as hex".to_owned(),
        });
    }
    let display = target.display.as_str();
    let (kind, object_id) = if let Some(raw) = bracket_value(display, "socket:") {
        (FdKind::Socket, parse_object_id(raw, "socket", &mut issues))
    } else if let Some(raw) = bracket_value(display, "pipe:") {
        (FdKind::Pipe, parse_object_id(raw, "pipe", &mut issues))
    } else if display.starts_with("anon_inode:") {
        (FdKind::AnonInode, None)
    } else if display.starts_with("/memfd:") || display.starts_with("memfd:") {
        (FdKind::Memfd, None)
    } else if display.starts_with('/') {
        (FdKind::Path, None)
    } else {
        (FdKind::Other, None)
    };
    let deleted = display.ends_with(" (deleted)");
    Parsed {
        value: NormalizedFdTarget {
            kind,
            target,
            object_id,
            deleted,
        },
        issues,
    }
}

#[must_use]
pub fn summarize_fd_targets(targets: &[NormalizedFdTarget]) -> FdSummary {
    let mut grouped = BTreeMap::<NormalizedFdTarget, u64>::new();
    for target in targets {
        *grouped.entry(target.clone()).or_default() += 1;
    }
    let groups = grouped
        .into_iter()
        .map(|(target, count)| FdGroup {
            kind: target.kind,
            target: target.target,
            object_id: target.object_id,
            deleted: target.deleted,
            count,
        })
        .collect();
    FdSummary {
        total: targets.len() as u64,
        groups,
    }
}

#[must_use]
pub fn parse_namespace_target(kind: &str, bytes: &[u8]) -> Parsed<Option<NamespaceIdentity>> {
    let encoded = EncodedValue::from_bytes(bytes);
    let mut issues = Vec::new();
    if encoded.raw_hex.is_some() {
        issues.push(ParseIssue {
            kind: ParseIssueKind::NonUtf8,
            line: None,
            field: Some(kind.to_owned()),
            detail: "namespace link target is not UTF-8".to_owned(),
        });
    }
    let expected_prefix = format!("{kind}:");
    let value = bracket_value(&encoded.display, &expected_prefix).and_then(|raw| {
        match raw.parse::<u64>() {
            Ok(inode) => Some(NamespaceIdentity {
                kind: kind.to_owned(),
                inode,
            }),
            Err(_) => None,
        }
    });
    if value.is_none() {
        issues.push(ParseIssue {
            kind: ParseIssueKind::InvalidValue,
            line: None,
            field: Some(kind.to_owned()),
            detail: "namespace target does not match kind:[inode]".to_owned(),
        });
    }
    Parsed { value, issues }
}

#[must_use]
pub fn parse_inet_table(input: &[u8], protocol: TransportProtocol) -> Parsed<Vec<NetworkEndpoint>> {
    let (text, mut issues) = input_text(input);
    let mut value = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if index == 0 && line.split_whitespace().next() == Some("sl") {
            continue;
        }
        let line_number = index + 1;
        let columns: Vec<_> = line.split_whitespace().collect();
        if columns.len() < 10 {
            issues.push(line_issue(
                ParseIssueKind::MalformedLine,
                line_number,
                None,
                "inet socket row has fewer than ten columns",
            ));
            continue;
        }
        if let Some(endpoint) = parse_inet_row(&columns, protocol, line_number, &mut issues) {
            value.push(endpoint);
        }
    }
    Parsed { value, issues }
}

fn parse_inet_row(
    columns: &[&str],
    protocol: TransportProtocol,
    line: usize,
    issues: &mut Vec<ParseIssue>,
) -> Option<NetworkEndpoint> {
    let local = parse_address(columns[1], line, "local_address", issues)?;
    let remote = parse_address(columns[2], line, "remote_address", issues)?;
    let state_code = u8::from_str_radix(columns[3], 16)
        .map_err(|_| invalid_network(line, "state", "invalid socket state", issues))
        .ok()?;
    let inode = columns[9]
        .parse()
        .map_err(|_| invalid_network(line, "inode", "invalid socket inode", issues))
        .ok()?;
    let state = socket_state(state_code);
    let remote_is_unspecified =
        remote.port == 0 && matches!(remote.address.as_str(), "0.0.0.0" | "::");
    let role = if protocol == TransportProtocol::Tcp && state == SocketState::Listen {
        NetworkRole::Listener
    } else if remote_is_unspecified {
        NetworkRole::Unconnected
    } else {
        NetworkRole::Connected
    };
    Some(NetworkEndpoint {
        protocol,
        role,
        state,
        local,
        remote,
        inode,
    })
}

fn parse_address(
    raw: &str,
    line: usize,
    field: &str,
    issues: &mut Vec<ParseIssue>,
) -> Option<NetworkAddress> {
    let Some((address, port)) = raw.split_once(':') else {
        invalid_network(line, field, "socket address has no port separator", issues);
        return None;
    };
    let port = u16::from_str_radix(port, 16)
        .map_err(|_| invalid_network(line, field, "invalid hexadecimal port", issues))
        .ok()?;
    let address = match address.len() {
        8 => parse_ipv4(address),
        32 => parse_ipv6(address),
        _ => None,
    };
    if let Some(address) = address {
        Some(NetworkAddress { address, port })
    } else {
        invalid_network(line, field, "invalid hexadecimal IP address", issues);
        None
    }
}

fn parse_ipv4(raw: &str) -> Option<String> {
    let mut bytes = decode_hex::<4>(raw)?;
    bytes.reverse();
    Some(std::net::Ipv4Addr::from(bytes).to_string())
}

fn parse_ipv6(raw: &str) -> Option<String> {
    let mut bytes = decode_hex::<16>(raw)?;
    for word in bytes.chunks_exact_mut(4) {
        word.reverse();
    }
    Some(Ipv6Addr::from(bytes).to_string())
}

fn decode_hex<const N: usize>(raw: &str) -> Option<[u8; N]> {
    if raw.len() != N * 2 {
        return None;
    }
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

fn socket_state(code: u8) -> SocketState {
    match code {
        0x01 => SocketState::Established,
        0x02 => SocketState::SynSent,
        0x03 => SocketState::SynReceived,
        0x04 => SocketState::FinWait1,
        0x05 => SocketState::FinWait2,
        0x06 => SocketState::TimeWait,
        0x07 => SocketState::Closed,
        0x08 => SocketState::CloseWait,
        0x09 => SocketState::LastAck,
        0x0a => SocketState::Listen,
        0x0b => SocketState::Closing,
        0x0c => SocketState::NewSynReceived,
        other => SocketState::Unknown(other),
    }
}

fn bracket_value<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .strip_prefix(prefix)?
        .strip_prefix('[')?
        .strip_suffix(']')
}

fn parse_object_id(raw: &str, field: &str, issues: &mut Vec<ParseIssue>) -> Option<u64> {
    raw.parse()
        .map_err(|_| {
            issues.push(ParseIssue {
                kind: ParseIssueKind::InvalidValue,
                line: None,
                field: Some(field.to_owned()),
                detail: "kernel object identifier is not an integer".to_owned(),
            });
        })
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

fn invalid_network(line: usize, field: &str, detail: &str, issues: &mut Vec<ParseIssue>) {
    issues.push(line_issue(
        ParseIssueKind::InvalidValue,
        line,
        Some(field),
        detail,
    ));
}

fn line_issue(kind: ParseIssueKind, line: usize, field: Option<&str>, detail: &str) -> ParseIssue {
    ParseIssue {
        kind,
        line: Some(line),
        field: field.map(str::to_owned),
        detail: detail.to_owned(),
    }
}
