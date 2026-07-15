"""Label-blind branch-value baselines."""

from __future__ import annotations

import hashlib
import json
import math
import os
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, List

from .schema import OPAQUE_ID_PATTERN, TrajectoryRecord

SCORE_SOURCES = frozenset({"sequence_encoder_v1", "random_v1", "heuristic_v1", "constant_train_v1"})


@dataclass(frozen=True)
class Prediction:
    trajectory_id: str
    success_probability: float
    value_score: float
    source: str

    def __post_init__(self) -> None:
        if (
            not isinstance(self.trajectory_id, str)
            or OPAQUE_ID_PATTERN.fullmatch(self.trajectory_id) is None
        ):
            raise ValueError("prediction trajectory_id must be 64 lowercase hexadecimal characters")
        for name, value in (
            ("success_probability", self.success_probability),
            ("value_score", self.value_score),
        ):
            if (
                isinstance(value, bool)
                or not isinstance(value, (int, float))
                or not math.isfinite(float(value))
                or not 0.0 <= value <= 1.0
            ):
                raise ValueError(f"{name} must be finite and in [0, 1]")
        if self.source not in SCORE_SOURCES:
            raise ValueError("prediction source is unsupported")

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
        succeeded = sum(event.status == "succeeded" for event in record.events)
        failed = sum(event.status in {"failed", "denied"} for event in record.events)
        unknown = sum(event.status == "unknown" for event in record.events)
        denominator = max(1, succeeded + failed + unknown)
        score = (succeeded + 0.25 * unknown) / denominator
        score = min(1.0, max(0.0, score))
        predictions.append(Prediction(record.trajectory_id, score, score, source="heuristic_v1"))
    return predictions


def constant_baseline(
    records: Sequence[TrajectoryRecord], success_probability: float, value_score: float
) -> List[Prediction]:
    """Apply constants learned from the training partition without reading evaluation labels."""

    return [
        Prediction(
            record.trajectory_id,
            success_probability,
            value_score,
            source="constant_train_v1",
        )
        for record in sorted(records, key=lambda item: item.trajectory_id)
    ]


def write_predictions_jsonl(path: os.PathLike[str], predictions: Sequence[Prediction]) -> None:
    """Create a private score-only JSONL file and refuse replacement."""

    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    ordered = sorted(predictions, key=lambda prediction: prediction.trajectory_id)
    if not ordered:
        raise ValueError("scores must contain at least one prediction")
    if len({prediction.trajectory_id for prediction in ordered}) != len(ordered):
        raise ValueError("scores contain duplicate trajectory identities")
    descriptor = None
    created = False
    try:
        try:
            descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            created = True
        except FileExistsError as error:
            raise ValueError(f"output already exists: {destination}") from error
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            descriptor = None
            for prediction in ordered:
                if not isinstance(prediction, Prediction):
                    raise ValueError("scores must contain typed Prediction records")
                output.write(
                    json.dumps(
                        prediction.as_dict(),
                        sort_keys=True,
                        separators=(",", ":"),
                        allow_nan=False,
                    )
                    + "\n"
                )
            output.flush()
            os.fsync(output.fileno())
    except Exception:
        if descriptor is not None:
            os.close(descriptor)
        if created:
            destination.unlink(missing_ok=True)
        raise


def _unit_digest(seed: int, trajectory_id: str, head: str) -> float:
    digest = hashlib.sha256(f"{seed}:{trajectory_id}:{head}".encode()).digest()
    integer = int.from_bytes(digest[:8], "big")
    return integer / float((1 << 64) - 1)
