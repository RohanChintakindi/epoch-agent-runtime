use epoch_proc::{
    CapabilityName, ParseIssueKind, decode_capability_mask, parse_cgroups, parse_maps, parse_status,
};

const VALID_STATUS: &[u8] = include_bytes!("fixtures/status-valid.txt");
const MALFORMED_STATUS: &[u8] = include_bytes!("fixtures/status-malformed.txt");
const VALID_MAPS: &[u8] = include_bytes!("fixtures/maps-valid.txt");
const MIXED_CGROUP: &[u8] = include_bytes!("fixtures/cgroup-mixed.txt");

#[test]
fn parses_selected_status_fields_and_decoded_capabilities() {
    let parsed = parse_status(VALID_STATUS);

    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    let status = parsed.value;
    assert_eq!(status.name.as_deref(), Some("epoch-agent"));
    assert_eq!(status.pid, Some(4242));
    assert_eq!(status.parent_pid, Some(4000));
    assert_eq!(status.namespace_pids, vec![4242, 7]);
    assert_eq!(status.thread_count, Some(3));
    assert_eq!(status.user_ids.expect("uids").effective, 1001);
    assert_eq!(status.group_ids.expect("gids").saved, 2002);
    assert_eq!(status.memory.vm_size_kib, Some(16384));
    assert_eq!(status.memory.rss_kib, Some(4096));
    assert_eq!(status.memory.swap_kib, Some(256));
    assert_eq!(status.no_new_privileges, Some(true));
    assert_eq!(status.seccomp_mode, Some(2));
    assert_eq!(
        status
            .capabilities
            .effective
            .expect("effective capabilities")
            .names,
        vec![
            CapabilityName::Chown,
            CapabilityName::NetBindService,
            CapabilityName::CheckpointRestore,
        ]
    );
}

#[test]
fn malformed_status_retains_valid_fields_and_reports_each_bad_field() {
    let parsed = parse_status(MALFORMED_STATUS);

    assert_eq!(parsed.value.parent_pid, Some(4000));
    assert_eq!(parsed.value.pid, None);
    assert_eq!(parsed.value.memory.rss_kib, None);
    assert!(parsed.issues.len() >= 7, "{:?}", parsed.issues);
    assert!(
        parsed
            .issues
            .iter()
            .any(|issue| issue.kind == ParseIssueKind::DuplicateField)
    );
    assert!(parsed.issues.iter().any(|issue| {
        issue.kind == ParseIssueKind::InvalidValue && issue.field.as_deref() == Some("Pid")
    }));
}

#[test]
fn capability_decoder_is_stable_and_retains_unknown_bits() {
    let decoded =
        decode_capability_mask((1_u64 << 0) | (1_u64 << 10) | (1_u64 << 40) | (1_u64 << 63));

    assert_eq!(decoded.raw_hex, "8000010000000401");
    assert_eq!(
        decoded.names,
        vec![
            CapabilityName::Chown,
            CapabilityName::NetBindService,
            CapabilityName::CheckpointRestore,
        ]
    );
    assert_eq!(decoded.unknown_bits, vec![63]);
}

#[test]
fn maps_summary_normalizes_addresses_and_paths_into_semantic_totals() {
    let parsed = parse_maps(VALID_MAPS);

    assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
    assert_eq!(parsed.value.region_count, 7);
    assert_eq!(parsed.value.mapped_bytes, 626_688);
    assert_eq!(parsed.value.executable_bytes, 339_968);
    assert_eq!(parsed.value.writable_private_bytes, 282_624);
    assert_eq!(parsed.value.file_backed_bytes, 344_064);
    assert_eq!(parsed.value.anonymous_bytes, 8_192);
    assert_eq!(parsed.value.special_bytes, 274_432);
    assert_eq!(parsed.value.shared_bytes, 4_096);
    assert_eq!(parsed.value.deleted_file_regions, 1);
}

#[test]
fn cgroup_parser_supports_v2_and_v1_and_diagnoses_bad_lines() {
    let parsed = parse_cgroups(MIXED_CGROUP);

    assert_eq!(parsed.value.len(), 3);
    assert_eq!(parsed.value[0].hierarchy_id, 0);
    assert!(parsed.value[0].controllers.is_empty());
    assert_eq!(
        parsed.value[1].controllers,
        vec!["cpu".to_owned(), "cpuacct".to_owned()]
    );
    assert_eq!(parsed.issues.len(), 1);
    assert_eq!(parsed.issues[0].kind, ParseIssueKind::MalformedLine);
}

#[test]
fn non_utf8_parser_input_is_diagnostic_not_a_panic() {
    let parsed = parse_status(b"Name:\tepoch\xffagent\nPid:\t7\nPPid:\t1\n");

    assert_eq!(parsed.value.pid, Some(7));
    assert!(
        parsed
            .issues
            .iter()
            .any(|issue| issue.kind == ParseIssueKind::NonUtf8)
    );
}
