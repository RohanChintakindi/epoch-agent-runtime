import json
import stat
from dataclasses import replace
from pathlib import Path

import pytest

from epoch_branch_value.schema import (
    KINDS,
    DatasetValidationError,
    TrajectoryEvent,
    TrajectoryRecord,
    TrajectorySummary,
    load_jsonl,
    write_jsonl,
)
from epoch_branch_value.synthetic import generate_records

EXPECTED_KINDS = {
    "agent.start",
    "context.update",
    "model.request",
    "model.response",
    "tool.call",
    "tool.result",
    "safe_point",
    "supervisor.run_started",
    "process.started",
    "process.manifest",
    "application.context_restored",
    "other",
}
RUST_SCHEMA_V1_FIXTURE = (
    Path(__file__).resolve().parents[3]
    / "crates"
    / "epoch-trajectory"
    / "tests"
    / "fixtures"
    / "schema-v1.jsonl"
)


def test_python_loads_the_canonical_rust_schema_v1_fixture():
    records = load_jsonl(RUST_SCHEMA_V1_FIXTURE)

    assert len(records) == 2
    assert any(record.is_labelled and record.events for record in records)
    assert any(not record.is_labelled and not record.events for record in records)
    assert {event.kind for record in records for event in record.events} == EXPECTED_KINDS
    assert {event.actor for record in records for event in record.events} == {
        "agent",
        "supervisor",
        "tool",
        "gateway",
        "operator",
    }


def test_real_rust_contract_round_trips_labelled_and_unlabelled_records_privately(tmp_path):
    records = generate_records(task_groups=4, branches_per_group=2, seed=17)
    records[0] = records[0].with_labels(success=None, value=None)
    dataset = tmp_path / "trajectories.jsonl"

    write_jsonl(dataset, records)

    assert load_jsonl(dataset) == records
    assert stat.S_IMODE(dataset.stat().st_mode) == 0o600
    assert dataset.read_bytes().endswith(b"\n")
    assert records[0].success_label is None
    assert records[0].value_label is None
    with pytest.raises(DatasetValidationError, match="already exists"):
        write_jsonl(dataset, records)


def test_record_output_exactly_matches_the_rust_metadata_only_schema():
    event = TrajectoryEvent(
        position=0,
        delta_monotonic_ns=0,
        actor="supervisor",
        kind="safe_point",
        status="succeeded",
        references_epoch=True,
        has_causal_parent=False,
    )
    record = TrajectoryRecord(
        schema_version=1,
        privacy_profile="metadata_only",
        trajectory_id="1" * 64,
        task_group_id="2" * 64,
        session_group_id="3" * 64,
        candidate_group_id="4" * 64,
        branch_depth=1,
        success_label=True,
        value_label=0.75,
        events=(event,),
        summary=TrajectorySummary.from_events((event,)),
    )

    encoded = record.as_dict()
    assert set(encoded) == {
        "schema_version",
        "privacy_profile",
        "trajectory_id",
        "task_group_id",
        "session_group_id",
        "candidate_group_id",
        "branch_depth",
        "success_label",
        "value_label",
        "events",
        "summary",
    }
    assert set(encoded["events"][0]) == {
        "position",
        "delta_monotonic_ns",
        "actor",
        "kind",
        "status",
        "references_epoch",
        "has_causal_parent",
    }
    assert set(encoded["summary"]) == {
        "event_count",
        "duration_monotonic_ns",
        "started_events",
        "succeeded_events",
        "failed_events",
        "denied_events",
        "unknown_events",
    }
    assert "branch_state" not in encoded


@pytest.mark.parametrize(
    ("mutation", "message"),
    [
        (lambda value: value.update(schema_version=2), "schema_version"),
        (lambda value: value.update(privacy_profile="full_text"), "privacy_profile"),
        (lambda value: value.update(raw_prompt="private"), "unknown field"),
        (lambda value: value.update(branch_state="failed"), "unknown field"),
        (lambda value: value.update(trajectory_id="a" * 63), "trajectory_id"),
        (lambda value: value.update(task_group_id="A" * 64), "task_group_id"),
        (lambda value: value.update(branch_depth=-1), "branch_depth"),
        (lambda value: value.update(success_label=None), "label pair"),
        (lambda value: value.update(value_label=None), "label pair"),
        (lambda value: value.update(value_label=float("nan")), "value_label"),
        (lambda value: value["events"][0].update(position=9), "contiguous"),
        (lambda value: value["events"][0].update(delta_monotonic_ns=1), "must be zero"),
        (lambda value: value["events"][0].update(delta_monotonic_ns=-1), "delta_monotonic_ns"),
        (lambda value: value["events"][0].update(actor="root"), "actor"),
        (lambda value: value["events"][0].update(kind="tool result"), "kind"),
        (lambda value: value["events"][0].update(kind="a" * 129), "kind"),
        (lambda value: value["events"][0].update(status="completed"), "status"),
        (lambda value: value["events"][0].update(references_epoch=1), "references_epoch"),
        (lambda value: value["summary"].update(event_count=99), "summary"),
        (lambda value: value["summary"].update(duration_monotonic_ns=99), "summary"),
    ],
)
def test_reader_rejects_contract_drift_and_inconsistent_derived_summary(
    tmp_path, mutation, message
):
    value = generate_records(task_groups=1, branches_per_group=1, seed=2)[0].as_dict()
    mutation(value)
    dataset = tmp_path / "invalid.jsonl"
    dataset.write_text(json.dumps(value, allow_nan=True) + "\n", encoding="utf-8")

    with pytest.raises(DatasetValidationError, match=message):
        load_jsonl(dataset)


def test_zero_events_are_valid_but_more_than_256_are_rejected(tmp_path):
    record = generate_records(task_groups=1, branches_per_group=1, seed=3)[0]
    empty = replace(record, events=(), summary=TrajectorySummary.from_events(()))
    dataset = tmp_path / "empty.jsonl"
    write_jsonl(dataset, [empty])
    assert load_jsonl(dataset) == [empty]

    invalid = record.as_dict()
    invalid["events"] = [{**record.events[0].as_dict(), "position": index} for index in range(257)]
    oversized = tmp_path / "oversized.jsonl"
    oversized.write_text(json.dumps(invalid) + "\n", encoding="utf-8")
    with pytest.raises(DatasetValidationError, match="events"):
        load_jsonl(oversized)


def test_reader_reports_line_numbers_duplicate_keys_and_trajectory_ids(tmp_path):
    record = generate_records(task_groups=1, branches_per_group=1, seed=2)[0]
    duplicate_key = tmp_path / "duplicate-key.jsonl"
    duplicate_key.write_text(
        '{"schema_version":1,"schema_version":1}\n',
        encoding="utf-8",
    )
    with pytest.raises(DatasetValidationError, match=r"line 1.*duplicate JSON key"):
        load_jsonl(duplicate_key)

    duplicate_id = tmp_path / "duplicate-id.jsonl"
    duplicate = replace(record, candidate_group_id="f" * 64)
    duplicate_id.write_text(
        json.dumps(record.as_dict()) + "\n" + json.dumps(duplicate.as_dict()) + "\n",
        encoding="utf-8",
    )
    with pytest.raises(DatasetValidationError, match=r"line 2.*duplicate trajectory_id"):
        load_jsonl(duplicate_id)


def test_reader_and_writer_reject_session_groups_crossing_task_groups(tmp_path):
    records = generate_records(task_groups=2, branches_per_group=1, seed=23)
    crossed = replace(records[1], session_group_id=records[0].session_group_id)
    assert crossed.task_group_id != records[0].task_group_id
    dataset = tmp_path / "crossed-session.jsonl"
    dataset.write_text(
        json.dumps(records[0].as_dict()) + "\n" + json.dumps(crossed.as_dict()) + "\n",
        encoding="utf-8",
    )

    with pytest.raises(DatasetValidationError, match=r"session_group_id.*task groups"):
        load_jsonl(dataset)
    with pytest.raises(DatasetValidationError, match=r"session_group_id.*task groups"):
        write_jsonl(tmp_path / "crossed-session-output.jsonl", [records[0], crossed])


def test_reader_normalizes_an_overflowing_integer_label(tmp_path):
    value = generate_records(task_groups=1, branches_per_group=1, seed=24)[0].as_dict()
    value["value_label"] = int("9" * 400)
    dataset = tmp_path / "overflowing-label.jsonl"
    dataset.write_text(json.dumps(value) + "\n", encoding="utf-8")

    with pytest.raises(DatasetValidationError, match="value_label"):
        load_jsonl(dataset)


def test_bounded_reader_and_typed_constructor_validation(tmp_path):
    assert KINDS == frozenset(EXPECTED_KINDS)
    with pytest.raises(DatasetValidationError, match="kind"):
        TrajectoryEvent(
            position=0,
            delta_monotonic_ns=0,
            actor="agent",
            kind="",
            status="unknown",
            references_epoch=False,
            has_causal_parent=False,
        )

    for kind in EXPECTED_KINDS:
        TrajectoryEvent(
            position=0,
            delta_monotonic_ns=0,
            actor="agent",
            kind=kind,
            status="unknown",
            references_epoch=False,
            has_causal_parent=False,
        )
    with pytest.raises(DatasetValidationError, match="kind"):
        TrajectoryEvent(
            position=0,
            delta_monotonic_ns=0,
            actor="agent",
            kind="made.up.category",
            status="unknown",
            references_epoch=False,
            has_causal_parent=False,
        )

    records = generate_records(task_groups=2, branches_per_group=1, seed=3)
    dataset = tmp_path / "bounded.jsonl"
    write_jsonl(dataset, records)
    with pytest.raises(DatasetValidationError, match="maximum record count"):
        load_jsonl(dataset, max_records=1)


def test_reader_uses_a_bounded_readline_instead_of_unbounded_iteration(monkeypatch, tmp_path):
    class GuardedStream:
        def __init__(self):
            self.calls = []

        def __enter__(self):
            return self

        def __exit__(self, *_):
            return False

        def __iter__(self):
            raise AssertionError("unbounded binary file iteration is forbidden")

        def readline(self, limit):
            self.calls.append(limit)
            return b"{" + b"x" * limit

    stream = GuardedStream()
    monkeypatch.setattr(Path, "open", lambda *_args, **_kwargs: stream)
    with pytest.raises(DatasetValidationError, match="record exceeds"):
        load_jsonl(tmp_path / "large-unterminated.jsonl")
    assert stream.calls == [256 * 1024 + 1]
