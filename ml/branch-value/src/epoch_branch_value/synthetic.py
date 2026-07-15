"""Deterministic metadata-only synthetic fixtures with noisy, nonterminal signal."""

from __future__ import annotations

import hashlib
import random
from typing import List

from .schema import (
    PRIVACY_PROFILE,
    SCHEMA_VERSION,
    TrajectoryEvent,
    TrajectoryRecord,
    TrajectorySummary,
)


def generate_records(
    task_groups: int = 30, branches_per_group: int = 2, seed: int = 101
) -> List[TrajectoryRecord]:
    if not 1 <= task_groups <= 10_000:
        raise ValueError("task_groups must be between 1 and 10000")
    if not 1 <= branches_per_group <= 256:
        raise ValueError("branches_per_group must be between 1 and 256")
    if isinstance(seed, bool) or not isinstance(seed, int) or seed < 0:
        raise ValueError("seed must be a nonnegative integer")
    records: List[TrajectoryRecord] = []
    for group_index in range(task_groups):
        task_group_id = _digest("task", seed, group_index)
        session_group_id = _digest("session", seed, group_index)
        candidate_group_id = _digest("candidate", seed, group_index)
        for branch_index in range(branches_per_group):
            randomizer = random.Random(f"{seed}:{group_index}:{branch_index}")
            success = randomizer.random() < 0.5
            noisy_failure = randomizer.random() < (0.3 if success else 0.7)
            events = _events(randomizer, noisy_failure)
            value_center = 0.78 if success else 0.22
            value = min(1.0, max(0.0, value_center + randomizer.uniform(-0.2, 0.2)))
            records.append(
                TrajectoryRecord(
                    schema_version=SCHEMA_VERSION,
                    privacy_profile=PRIVACY_PROFILE,
                    trajectory_id=_digest("trajectory", seed, group_index, branch_index),
                    task_group_id=task_group_id,
                    session_group_id=session_group_id,
                    candidate_group_id=candidate_group_id,
                    branch_depth=randomizer.randint(0, 3),
                    success_label=success,
                    value_label=value,
                    events=events,
                    summary=TrajectorySummary.from_events(events),
                )
            )
    return records


def _events(randomizer: random.Random, noisy_failure: bool) -> tuple:
    return (
        TrajectoryEvent(
            position=0,
            delta_monotonic_ns=0,
            actor="supervisor",
            kind="supervisor.run_started",
            status="started",
            references_epoch=False,
            has_causal_parent=False,
        ),
        TrajectoryEvent(
            position=1,
            delta_monotonic_ns=randomizer.randint(10_000, 200_000),
            actor="agent",
            kind="model.request",
            status="started",
            references_epoch=False,
            has_causal_parent=True,
        ),
        TrajectoryEvent(
            position=2,
            delta_monotonic_ns=randomizer.randint(50_000, 900_000),
            actor="tool",
            kind="tool.result",
            status="failed" if noisy_failure else "succeeded",
            references_epoch=True,
            has_causal_parent=True,
        ),
        TrajectoryEvent(
            position=3,
            delta_monotonic_ns=randomizer.randint(10_000, 200_000),
            actor="supervisor",
            kind="safe_point",
            status="unknown",
            references_epoch=True,
            has_causal_parent=True,
        ),
    )


def _digest(namespace: str, *parts: int) -> str:
    digest = hashlib.sha256()
    digest.update(b"epoch-branch-value-synthetic-v1\0")
    digest.update(namespace.encode("ascii"))
    for part in parts:
        digest.update(b"\0")
        digest.update(str(part).encode("ascii"))
    return digest.hexdigest()
