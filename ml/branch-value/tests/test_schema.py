import json
from dataclasses import replace

import pytest

from epoch_branch_value.schema import (
    DatasetValidationError,
    Label,
    Step,
    TrajectoryRecord,
    load_jsonl,
    write_jsonl,
)
from epoch_branch_value.synthetic import generate_records


def test_versioned_jsonl_round_trips_typed_records(tmp_path):
    records = generate_records(task_groups=4, branches_per_group=2, seed=17)
    dataset = tmp_path / "trajectories.jsonl"
    write_jsonl(dataset, records)

    loaded = load_jsonl(dataset)

    assert loaded == records
    assert all(record.schema_version == 1 for record in loaded)
    assert dataset.read_bytes().endswith(b"\n")


@pytest.mark.parametrize(
    ("mutation", "message"),
    [
        (lambda value: value.update(schema_version=2), "schema_version"),
        (lambda value: value.update(raw_prompt="private"), "unknown field"),
        (lambda value: value["steps"][0].update(text="secret"), "unknown field"),
        (lambda value: value.update(task_group_id="alice@example.com"), "task_group_id"),
        (lambda value: value["steps"][0].update(sequence=9), "contiguous"),
        (lambda value: value["label"].update(value=1.1), "label.value"),
        (lambda value: value.update(steps=[]), "steps"),
    ],
)
def test_reader_rejects_unversioned_unbounded_or_text_bearing_data(tmp_path, mutation, message):
    record = generate_records(task_groups=1, branches_per_group=1, seed=2)[0].as_dict()
    mutation(record)
    dataset = tmp_path / "invalid.jsonl"
    dataset.write_text(json.dumps(record) + "\n", encoding="utf-8")

    with pytest.raises(DatasetValidationError, match=message):
        load_jsonl(dataset)


def test_reader_reports_line_numbers_duplicate_keys_and_duplicate_trajectory_ids(tmp_path):
    record = generate_records(task_groups=1, branches_per_group=1, seed=2)[0]
    duplicate_key = tmp_path / "duplicate-key.jsonl"
    duplicate_key.write_text(
        '{"schema_version":1,"schema_version":1}\n',
        encoding="utf-8",
    )
    with pytest.raises(DatasetValidationError, match=r"line 1.*duplicate JSON key"):
        load_jsonl(duplicate_key)

    duplicate_id = tmp_path / "duplicate-id.jsonl"
    duplicate = replace(record, branch_id="00000000-0000-4000-8000-000000000099")
    duplicate_id.write_text(
        json.dumps(record.as_dict()) + "\n" + json.dumps(duplicate.as_dict()) + "\n",
        encoding="utf-8",
    )
    with pytest.raises(DatasetValidationError, match=r"line 2.*duplicate trajectory_id"):
        load_jsonl(duplicate_id)


def test_typed_constructors_enforce_bounds_without_json(tmp_path):
    with pytest.raises(DatasetValidationError, match="duration_ms"):
        Step(
            sequence=0,
            actor="agent",
            kind="context.update",
            status="succeeded",
            duration_ms=-1.0,
            token_count=1,
            effect_count=0,
            capability_count=0,
        )
    with pytest.raises(DatasetValidationError, match=r"label.value"):
        Label(success=True, value=float("nan"))

    records = generate_records(task_groups=2, branches_per_group=1, seed=3)
    with pytest.raises(DatasetValidationError, match="maximum record count"):
        load_jsonl(_write(tmp_path, records), max_records=1)


def _write(tmp_path, records):
    path = tmp_path / "bounded.jsonl"
    write_jsonl(path, records)
    return path


def test_record_output_has_an_exact_non_authoritative_schema():
    record = TrajectoryRecord(
        schema_version=1,
        trajectory_id="00000000-0000-4000-8000-000000000001",
        task_group_id="tg_0123456789abcdef",
        branch_id="00000000-0000-4000-8000-000000000002",
        parent_branch_id=None,
        steps=(
            Step(
                sequence=0,
                actor="supervisor",
                kind="safe_point",
                status="succeeded",
                duration_ms=1.0,
                token_count=0,
                effect_count=0,
                capability_count=0,
            ),
        ),
        label=Label(success=True, value=1.0),
    )
    assert set(record.as_dict()) == {
        "schema_version",
        "trajectory_id",
        "task_group_id",
        "branch_id",
        "parent_branch_id",
        "steps",
        "label",
    }
