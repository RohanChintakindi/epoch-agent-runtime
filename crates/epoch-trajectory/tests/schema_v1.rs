use std::collections::BTreeSet;

use epoch_trajectory::{
    BranchTrajectory, PrivacyProfile, TRAJECTORY_SCHEMA_VERSION, TrajectoryEvent, TrajectorySummary,
};

const SCHEMA_V1_JSONL: &str = include_str!("fixtures/schema-v1.jsonl");
const ACTORS: [&str; 5] = ["agent", "gateway", "operator", "supervisor", "tool"];
const STATUSES: [&str; 5] = ["denied", "failed", "started", "succeeded", "unknown"];
const EVENT_KINDS: [&str; 13] = [
    "agent.start",
    "application.context_restored",
    "context.update",
    "model.request",
    "model.response",
    "other",
    "process.manifest",
    "process.started",
    "process.stderr",
    "safe_point",
    "supervisor.run_started",
    "tool.call",
    "tool.result",
];

#[test]
fn schema_v1_fixture_obeys_semantic_contract() {
    let records = fixture_records();
    let mut actors = BTreeSet::new();
    let mut statuses = BTreeSet::new();
    let mut event_kinds = BTreeSet::new();
    let mut saw_labelled_nonempty = false;
    let mut saw_unlabelled_empty = false;

    for (_, record) in &records {
        assert_eq!(record.schema_version, TRAJECTORY_SCHEMA_VERSION);
        assert_eq!(record.privacy_profile, PrivacyProfile::MetadataOnly);
        assert_pseudonym(&record.trajectory_id);
        assert_pseudonym(&record.task_group_id);
        assert_pseudonym(&record.session_group_id);
        assert_pseudonym(&record.candidate_group_id);

        match (record.success_label, record.value_label) {
            (Some(_), Some(value)) => {
                assert!(value.is_finite(), "value label must be finite");
                assert!(
                    (0.0..=1.0).contains(&value),
                    "value label must be normalized"
                );
                saw_labelled_nonempty |= !record.events.is_empty();
            }
            (None, None) => saw_unlabelled_empty |= record.events.is_empty(),
            _ => panic!("success and value labels must either both be present or both be absent"),
        }

        for (expected_position, event) in record.events.iter().enumerate() {
            assert_eq!(
                event.position,
                u64::try_from(expected_position).expect("fixture position fits in u64")
            );
            assert!(ACTORS.contains(&event.actor.as_str()));
            assert!(STATUSES.contains(&event.status.as_str()));
            assert!(EVENT_KINDS.contains(&event.kind.as_str()));
            actors.insert(event.actor.as_str());
            statuses.insert(event.status.as_str());
            event_kinds.insert(event.kind.as_str());
        }
        assert_eq!(record.summary, summarize(&record.events));
    }

    assert!(saw_labelled_nonempty);
    assert!(saw_unlabelled_empty);
    assert_eq!(actors, ACTORS.into_iter().collect());
    assert_eq!(statuses, STATUSES.into_iter().collect());
    assert_eq!(event_kinds, EVENT_KINDS.into_iter().collect());
}

#[test]
fn schema_v1_fixture_has_stable_canonical_reserialization() {
    assert!(SCHEMA_V1_JSONL.ends_with('\n'));
    for (line, record) in fixture_records() {
        let reserialized =
            serde_json::to_string(&record).expect("serialize schema-v1 fixture record");
        assert_eq!(reserialized, line);
        let reparsed: BranchTrajectory =
            serde_json::from_str(&reserialized).expect("reparse schema-v1 fixture record");
        assert_eq!(reparsed, record);
    }
}

fn fixture_records() -> Vec<(&'static str, BranchTrajectory)> {
    SCHEMA_V1_JSONL
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let record = serde_json::from_str::<BranchTrajectory>(line).unwrap_or_else(|error| {
                panic!("invalid schema-v1 fixture line {}: {error}", index + 1)
            });
            (line, record)
        })
        .collect()
}

fn assert_pseudonym(value: &str) {
    assert_eq!(value.len(), 64);
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "pseudonym must be 64 lowercase hexadecimal characters"
    );
}

fn summarize(events: &[TrajectoryEvent]) -> TrajectorySummary {
    let mut summary = TrajectorySummary {
        event_count: u64::try_from(events.len()).expect("fixture event count fits in u64"),
        ..TrajectorySummary::default()
    };
    for event in events {
        summary.duration_monotonic_ns = summary
            .duration_monotonic_ns
            .checked_add(event.delta_monotonic_ns)
            .expect("fixture duration fits in u64");
        match event.status.as_str() {
            "started" => summary.started_events += 1,
            "succeeded" => summary.succeeded_events += 1,
            "failed" => summary.failed_events += 1,
            "denied" => summary.denied_events += 1,
            "unknown" => summary.unknown_events += 1,
            status => panic!("unsupported fixture status {status:?}"),
        }
    }
    summary
}
