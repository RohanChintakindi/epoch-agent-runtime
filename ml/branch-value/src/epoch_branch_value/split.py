"""Deterministic labelled-only task-group splits with sibling leakage checks."""

from __future__ import annotations

import math
import random
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from typing import Any, Dict, Tuple

from .schema import TrajectoryRecord

SPLIT_NAMES = ("train", "validation", "test")


@dataclass(frozen=True)
class SplitConfig:
    seed: int = 17
    train_ratio: float = 0.7
    validation_ratio: float = 0.15

    @property
    def test_ratio(self) -> float:
        return 1.0 - self.train_ratio - self.validation_ratio

    def validate(self) -> None:
        if isinstance(self.seed, bool) or not isinstance(self.seed, int) or self.seed < 0:
            raise ValueError("split seed must be a nonnegative integer")
        if (
            isinstance(self.train_ratio, bool)
            or not isinstance(self.train_ratio, (int, float))
            or not math.isfinite(self.train_ratio)
            or not 0.0 < self.train_ratio < 1.0
        ):
            raise ValueError("train_ratio must be finite and between zero and one")
        if (
            isinstance(self.validation_ratio, bool)
            or not isinstance(self.validation_ratio, (int, float))
            or not math.isfinite(self.validation_ratio)
            or not 0.0 <= self.validation_ratio < 1.0
        ):
            raise ValueError("validation_ratio must be finite and in [0, 1)")
        if not math.isfinite(self.test_ratio) or not 0.0 < self.test_ratio < 1.0:
            raise ValueError("train, validation, and test ratios must sum to one")

    def as_dict(self) -> Dict[str, Any]:
        return {
            "seed": self.seed,
            "train_ratio": self.train_ratio,
            "validation_ratio": self.validation_ratio,
            "test_ratio": self.test_ratio,
        }

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> SplitConfig:
        if not isinstance(value, dict) or set(value) != {
            "seed",
            "train_ratio",
            "validation_ratio",
            "test_ratio",
        }:
            raise ValueError("persisted split config is invalid")
        config = cls(
            seed=value["seed"],
            train_ratio=value["train_ratio"],
            validation_ratio=value["validation_ratio"],
        )
        config.validate()
        persisted_test = value["test_ratio"]
        if (
            isinstance(persisted_test, bool)
            or not isinstance(persisted_test, (int, float))
            or not math.isfinite(float(persisted_test))
            or float(persisted_test) != config.test_ratio
        ):
            raise ValueError("persisted split test ratio is inconsistent")
        return config


@dataclass(frozen=True)
class DatasetSplit:
    config: SplitConfig
    assignment: Mapping[str, str]
    group_assignment: Mapping[str, str]

    def groups(self, split: str) -> Tuple[str, ...]:
        _validate_split_name(split)
        return tuple(
            sorted(group for group, assigned in self.group_assignment.items() if assigned == split)
        )

    def record_ids(self, split: str) -> Tuple[str, ...]:
        _validate_split_name(split)
        return tuple(
            sorted(
                trajectory_id
                for trajectory_id, assigned in self.assignment.items()
                if assigned == split
            )
        )

    def as_dict(self) -> Dict[str, Any]:
        return {
            "schema_version": 1,
            "unit": "task_group_id",
            "config": self.config.as_dict(),
            "group_assignment": dict(sorted(self.group_assignment.items())),
            "assignment": dict(sorted(self.assignment.items())),
        }


def split_by_task_group(records: Sequence[TrajectoryRecord], config: SplitConfig) -> DatasetSplit:
    config.validate()
    if not records:
        raise ValueError("cannot split an empty dataset")
    trajectories = set()
    candidate_tasks: Dict[str, str] = {}
    session_tasks: Dict[str, str] = {}
    for record in records:
        if record.trajectory_id in trajectories:
            raise ValueError(f"duplicate trajectory_id {record.trajectory_id}")
        trajectories.add(record.trajectory_id)
        previous = candidate_tasks.setdefault(record.candidate_group_id, record.task_group_id)
        if previous != record.task_group_id:
            raise ValueError("candidate_group_id appears in multiple task groups")
        previous = session_tasks.setdefault(record.session_group_id, record.task_group_id)
        if previous != record.task_group_id:
            raise ValueError("session_group_id appears in multiple task groups")
    labelled = [record for record in records if record.is_labelled]
    task_groups = {record.task_group_id for record in labelled}
    if len(task_groups) < 3:
        raise ValueError("at least three labelled task groups are required")
    ordered_groups = sorted(task_groups)
    random.Random(config.seed).shuffle(ordered_groups)
    train_count, validation_count = _partition_counts(len(ordered_groups), config)
    group_assignment: Dict[str, str] = {}
    for index, group in enumerate(ordered_groups):
        if index < train_count:
            assigned = "train"
        elif index < train_count + validation_count:
            assigned = "validation"
        else:
            assigned = "test"
        group_assignment[group] = assigned
    assignment = {
        record.trajectory_id: group_assignment[record.task_group_id]
        for record in sorted(labelled, key=lambda item: item.trajectory_id)
    }
    split = DatasetSplit(config, assignment, group_assignment)
    if any(not split.record_ids(name) for name in SPLIT_NAMES):
        raise ValueError("labelled task-group split produced an empty partition")
    return split


def records_for_split(
    records: Iterable[TrajectoryRecord], split: DatasetSplit, name: str
) -> Tuple[TrajectoryRecord, ...]:
    identifiers = set(split.record_ids(name))
    return tuple(
        sorted(
            (record for record in records if record.trajectory_id in identifiers),
            key=lambda record: record.trajectory_id,
        )
    )


def _partition_counts(group_count: int, config: SplitConfig) -> Tuple[int, int]:
    train = max(1, int(group_count * config.train_ratio))
    validation = max(1, int(group_count * config.validation_ratio))
    while train + validation >= group_count:
        if train > validation and train > 1:
            train -= 1
        elif validation > 1:
            validation -= 1
        else:
            raise ValueError("split ratios cannot allocate three nonempty partitions")
    return train, validation


def _validate_split_name(split: str) -> None:
    if split not in SPLIT_NAMES:
        raise ValueError(f"split must be one of {SPLIT_NAMES}")
