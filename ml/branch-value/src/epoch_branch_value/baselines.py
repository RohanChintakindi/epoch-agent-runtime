"""Label-blind branch-value baselines."""

from __future__ import annotations

import hashlib
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any, Dict, List

from .schema import TrajectoryRecord


@dataclass(frozen=True)
class Prediction:
    trajectory_id: str
    success_probability: float
    value_score: float
    source: str

    def __post_init__(self) -> None:
        for name, value in (
            ("success_probability", self.success_probability),
            ("value_score", self.value_score),
        ):
            if not 0.0 <= value <= 1.0:
                raise ValueError(f"{name} must be in [0, 1]")

    def as_dict(self) -> Dict[str, Any]:
        return {
            "trajectory_id": self.trajectory_id,
            "success_probability": self.success_probability,
            "value_score": self.value_score,
            "source": self.source,
        }


def random_baseline(records: Sequence[TrajectoryRecord], seed: int) -> List[Prediction]:
    """Return per-identity pseudorandom scores independent of input order and labels."""

    return [
        Prediction(
            trajectory_id=record.trajectory_id,
            success_probability=_unit_digest(seed, record.trajectory_id, "success"),
            value_score=_unit_digest(seed, record.trajectory_id, "value"),
            source="random_v1",
        )
        for record in sorted(records, key=lambda item: item.trajectory_id)
    ]


def heuristic_baseline(records: Sequence[TrajectoryRecord]) -> List[Prediction]:
    """Score normalized event outcomes without consulting labels or arbitrary text."""

    predictions = []
    for record in sorted(records, key=lambda item: item.trajectory_id):
        succeeded = sum(step.status == "succeeded" for step in record.steps)
        failed = sum(step.status in {"failed", "denied"} for step in record.steps)
        unknown = sum(step.status == "unknown" for step in record.steps)
        denominator = max(1, succeeded + failed + unknown)
        score = (succeeded + 0.25 * unknown) / denominator
        score = min(1.0, max(0.0, score))
        predictions.append(Prediction(record.trajectory_id, score, score, source="heuristic_v1"))
    return predictions


def _unit_digest(seed: int, trajectory_id: str, head: str) -> float:
    digest = hashlib.sha256(f"{seed}:{trajectory_id}:{head}".encode()).digest()
    integer = int.from_bytes(digest[:8], "big")
    return integer / float((1 << 64) - 1)
