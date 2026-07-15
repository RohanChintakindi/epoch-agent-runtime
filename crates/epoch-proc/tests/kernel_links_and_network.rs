use epoch_proc::{
    EncodedValue, FdKind, NetworkRole, ParseIssueKind, SocketState, TransportProtocol,
    normalize_fd_target, parse_inet_table, parse_namespace_target, summarize_fd_targets,
};

const TCP4: &[u8] = include_bytes!("fixtures/tcp4-mixed.txt");
const TCP6: &[u8] = include_bytes!("fixtures/tcp6-valid.txt");

#[test]
fn fd_targets_are_semantic_and_do_not_depend_on_fd_numbers() {
    let socket = normalize_fd_target(b"socket:[12345]");
    let pipe = normalize_fd_target(b"pipe:[77]");
    let anon = normalize_fd_target(b"anon_inode:[eventpoll]");
    let path = normalize_fd_target(b"/tmp/report.txt (deleted)");

    assert_eq!(socket.value.kind, FdKind::Socket);
    assert_eq!(socket.value.object_id, Some(12345));
    assert_eq!(pipe.value.kind, FdKind::Pipe);
    assert_eq!(pipe.value.object_id, Some(77));
    assert_eq!(anon.value.kind, FdKind::AnonInode);
    assert_eq!(path.value.kind, FdKind::Path);
    assert!(path.value.deleted);
    assert!(socket.issues.is_empty());
}

#[test]
fn non_utf8_fd_links_keep_exact_bytes_with_a_diagnostic() {
    let parsed = normalize_fd_target(b"/tmp/secret-\xff");

    assert_eq!(parsed.value.target.display, "/tmp/secret-�");
    assert_eq!(
        parsed.value.target.raw_hex.as_deref(),
        Some("2f746d702f7365637265742dff")
    );
    assert_eq!(parsed.issues.len(), 1);
    assert_eq!(parsed.issues[0].kind, ParseIssueKind::NonUtf8);
}

#[test]
fn fd_summary_groups_equivalent_targets_and_sorts_stably() {
    let inputs = [
        normalize_fd_target(b"socket:[12345]").value,
        normalize_fd_target(b"/tmp/a").value,
        normalize_fd_target(b"socket:[12345]").value,
        normalize_fd_target(b"pipe:[8]").value,
    ];
    let summary = summarize_fd_targets(&inputs);

    assert_eq!(summary.total, 4);
    assert_eq!(summary.groups.len(), 3);
    let socket = summary
        .groups
        .iter()
        .find(|group| group.kind == FdKind::Socket)
        .expect("socket group");
    assert_eq!(socket.count, 2);
    assert_eq!(socket.object_id, Some(12345));
}

#[test]
fn namespace_links_become_typed_inode_identities() {
    let parsed = parse_namespace_target("mnt", b"mnt:[4026531840]");

    let namespace = parsed.value.as_ref().expect("namespace");
    assert_eq!(namespace.kind, "mnt");
    assert_eq!(namespace.inode, 4_026_531_840);
    assert!(parsed.issues.is_empty());

    let malformed = parse_namespace_target("net", b"not-a-namespace");
    assert!(malformed.value.is_none());
    assert_eq!(malformed.issues[0].kind, ParseIssueKind::InvalidValue);
}

#[test]
fn tcp4_table_decodes_addresses_roles_states_and_retains_good_rows() {
    let parsed = parse_inet_table(TCP4, TransportProtocol::Tcp);

    assert_eq!(parsed.value.len(), 2);
    assert_eq!(parsed.issues.len(), 1);
    assert_eq!(parsed.issues[0].kind, ParseIssueKind::MalformedLine);
    assert_eq!(parsed.value[0].local.address, "127.0.0.1");
    assert_eq!(parsed.value[0].local.port, 8080);
    assert_eq!(parsed.value[0].state, SocketState::Listen);
    assert_eq!(parsed.value[0].role, NetworkRole::Listener);
    assert_eq!(parsed.value[0].inode, 12345);
    assert_eq!(parsed.value[1].local.address, "10.0.0.1");
    assert_eq!(parsed.value[1].remote.address, "8.8.8.8");
    assert_eq!(parsed.value[1].remote.port, 443);
    assert_eq!(parsed.value[1].state, SocketState::Established);
    assert_eq!(parsed.value[1].role, NetworkRole::Connected);
}

#[test]
fn tcp6_word_endianness_is_normalized() {
    let parsed = parse_inet_table(TCP6, TransportProtocol::Tcp);

    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    assert_eq!(parsed.value[0].local.address, "::1");
    assert_eq!(parsed.value[0].local.port, 8443);
    assert_eq!(parsed.value[0].remote.address, "::");
}

#[test]
fn encoded_value_utf8_has_no_redundant_raw_copy() {
    let value = EncodedValue::from_bytes(b"plain");

    assert_eq!(value.display, "plain");
    assert_eq!(value.raw_hex, None);
}
