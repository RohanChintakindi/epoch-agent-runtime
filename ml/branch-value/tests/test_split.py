from dataclasses import replace

import pytest

from epoch_branch_value.split import SplitConfig, split_by_task_group
from epoch_branch_value.synthetic import generate_records


def test_split_is_group_disjoint_deterministic_and_input_order_independent():
    records = generate_records(task_groups=20, branches_per_group=3, seed=9)
    config = SplitConfig(seed=42, train_ratio=0.7, validation_ratio=0.15)

    first = split_by_task_group(records, config)
    second = split_by_task_group(list(reversed(records)), config)

    assert first.as_dict() == second.as_dict()
    group_sets = [set(first.groups(name)) for name in ("train", "validation", "test")]
    assert group_sets[0].isdisjoint(group_sets[1])
    assert group_sets[0].isdisjoint(group_sets[2])
    assert group_sets[1].isdisjoint(group_sets[2])
    assert set.union(*group_sets) == {record.task_group_id for record in records}
    for task_group in {record.task_group_id for record in records}:
        assigned = {
            first.assignment[record.trajectory_id]
            for record in records
            if record.task_group_id == task_group
        }
        assert len(assigned) == 1


def test_split_seed_changes_assignment_but_never_group_integrity():
    records = generate_records(task_groups=30, branches_per_group=2, seed=5)
    first = split_by_task_group(records, SplitConfig(seed=1))
    second = split_by_task_group(records, SplitConfig(seed=2))
    assert first.assignment != second.assignment


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


def test_conflicting_task_group_for_duplicate_branch_identity_is_rejected():
    records = generate_records(task_groups=4, branches_per_group=1, seed=4)
    poisoned = replace(
        records[1],
        branch_id=records[0].branch_id,
        task_group_id="tg_ffffffffffffffff",
    )
    with pytest.raises(ValueError, match=r"branch_id.*task groups"):
        split_by_task_group([records[0], poisoned, *records[2:]], SplitConfig(seed=3))
