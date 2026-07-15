from dataclasses import replace

import pytest

from epoch_branch_value.split import SplitConfig, split_by_task_group
from epoch_branch_value.synthetic import generate_records


def test_split_uses_only_labelled_task_groups_and_is_deterministic():
    records = generate_records(task_groups=20, branches_per_group=3, seed=9)
    for index in range(0, len(records), 3):
        records[index] = records[index].with_labels(success=None, value=None)
    config = SplitConfig(seed=42, train_ratio=0.7, validation_ratio=0.15)

    first = split_by_task_group(records, config)
    second = split_by_task_group(list(reversed(records)), config)

    assert first.as_dict() == second.as_dict()
    assert len(first.assignment) == 40
    assert all(records_by_id(records)[identifier].is_labelled for identifier in first.assignment)
    group_sets = [set(first.groups(name)) for name in ("train", "validation", "test")]
    assert group_sets[0].isdisjoint(group_sets[1])
    assert group_sets[0].isdisjoint(group_sets[2])
    assert group_sets[1].isdisjoint(group_sets[2])


def test_unlabelled_only_groups_are_excluded_and_fewer_than_three_labelled_groups_fail():
    records = generate_records(task_groups=4, branches_per_group=2, seed=5)
    unlabelled_group = records[0].task_group_id
    records = [
        record.with_labels(success=None, value=None)
        if record.task_group_id == unlabelled_group
        else record
        for record in records
    ]
    split = split_by_task_group(records, SplitConfig(seed=1))
    assert unlabelled_group not in split.group_assignment

    too_few = [
        record.with_labels(success=None, value=None)
        if record.task_group_id not in {records[2].task_group_id, records[4].task_group_id}
        else record
        for record in records
    ]
    with pytest.raises(ValueError, match="at least three labelled task groups"):
        split_by_task_group(too_few, SplitConfig(seed=1))


def test_split_rejects_duplicate_trajectories_and_candidate_groups_crossing_tasks():
    records = generate_records(task_groups=4, branches_per_group=2, seed=22)
    with pytest.raises(ValueError, match="duplicate trajectory_id"):
        split_by_task_group([records[0], records[0], *records[1:]], SplitConfig(seed=2))

    crossed = replace(records[2], candidate_group_id=records[0].candidate_group_id)
    assert crossed.task_group_id != records[0].task_group_id
    with pytest.raises(ValueError, match=r"candidate_group_id.*task groups"):
        split_by_task_group([records[0], records[1], crossed, *records[3:]], SplitConfig(seed=2))


@pytest.mark.parametrize(
    "config",
    [
        SplitConfig(train_ratio=0.0, validation_ratio=0.2),
        SplitConfig(train_ratio=0.9, validation_ratio=0.2),
        SplitConfig(train_ratio=0.8, validation_ratio=-0.1),
    ],
)
def test_invalid_split_ratios_fail_closed(config):
    with pytest.raises(ValueError):
        split_by_task_group(generate_records(4, 1, 1), config)


def records_by_id(records):
    return {record.trajectory_id: record for record in records}
