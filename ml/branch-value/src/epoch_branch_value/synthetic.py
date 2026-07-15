"""Deterministic, credential-free synthetic branch fixtures."""

from __future__ import annotations

import hashlib
import random
import uuid
from typing import List

from .schema import Label, Step, TrajectoryRecord


def generate_records(
    task_groups: int, branches_per_group: int, seed: int
) -> List[TrajectoryRecord]:
    if not 1 <= task_groups <= 10_000:
        raise ValueError("task_groups must be between 1 and 10000")
    if not 1 <= branches_per_group <= 32:
        raise ValueError("branches_per_group must be between 1 and 32")
    if isinstance(seed, bool) or not isinstance(seed, int) or seed < 0:
        raise ValueError("seed must be a nonnegative integer")
    randomizer = random.Random(seed)
    records: List[TrajectoryRecord] = []
    for group_index in range(task_groups):
        group_digest = hashlib.sha256(
            f"epoch.synthetic.group:{seed}:{group_index}".encode()
        ).hexdigest()
        task_group_id = f"tg_{group_digest[:24]}"
        parent_branch_id = _stable_uuid(seed, group_index, -1, "parent")
        preferred_branch = group_index % branches_per_group
        for branch_index in range(branches_per_group):
            success = branch_index == preferred_branch
            duration = 4.0 + randomizer.random() * 8.0 + branch_index
            token_count = 24 + group_index % 11 + branch_index * 3
            terminal_status = "succeeded" if success else "failed"
            value = (
                0.85 + randomizer.random() * 0.1 if success else 0.05 + randomizer.random() * 0.2
            )
            steps = (
                Step(0, "supervisor", "supervisor.run_started", "started", 0.1, 0, 0, 0),
                Step(1, "agent", "context.update", "succeeded", duration, token_count, 0, 0),
                Step(2, "tool", "tool.result", terminal_status, duration / 2, 0, 0, 0),
                Step(3, "supervisor", "safe_point", terminal_status, 0.2, 0, 0, 0),
            )
            records.append(
                TrajectoryRecord(
                    schema_version=1,
                    trajectory_id=_stable_uuid(seed, group_index, branch_index, "trajectory"),
                    task_group_id=task_group_id,
                    branch_id=_stable_uuid(seed, group_index, branch_index, "branch"),
                    parent_branch_id=parent_branch_id,
                    steps=steps,
                    label=Label(success=success, value=value),
                )
            )
    return records


def _stable_uuid(seed: int, group_index: int, branch_index: int, namespace: str) -> str:
    digest = hashlib.sha256(
        f"epoch.synthetic:{namespace}:{seed}:{group_index}:{branch_index}".encode()
    ).digest()
    return str(uuid.UUID(bytes=digest[:16], version=4))
