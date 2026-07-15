"""Small dependency-free evaluation metrics for identical model/baseline comparisons."""

from __future__ import annotations

import math
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any, Dict

from .baselines import Prediction
from .schema import TrajectoryRecord


@dataclass(frozen=True)
class Metrics:
    count: int
    success_accuracy: float
    success_brier: float
    success_log_loss: float
    value_mae: float
    value_rmse: float

    def as_dict(self) -> Dict[str, Any]:
        return {
            "count": self.count,
            "success_accuracy": self.success_accuracy,
            "success_brier": self.success_brier,
            "success_log_loss": self.success_log_loss,
            "value_mae": self.value_mae,
            "value_rmse": self.value_rmse,
        }


def evaluate_predictions(
    records: Sequence[TrajectoryRecord], predictions: Sequence[Prediction]
) -> Metrics:
    if not records:
        raise ValueError("metrics require at least one record")
    by_id = {prediction.trajectory_id: prediction for prediction in predictions}
    if len(by_id) != len(predictions):
        raise ValueError("predictions contain duplicate trajectory identities")
    expected = {record.trajectory_id for record in records}
    if set(by_id) != expected:
        raise ValueError("predictions do not exactly match evaluation records")
    correct = 0
    brier = 0.0
    log_loss = 0.0
    absolute_error = 0.0
    squared_error = 0.0
    epsilon = 1e-7
    for record in records:
        if not record.is_labelled:
            raise ValueError("metrics require labelled records")
        prediction = by_id[record.trajectory_id]
        target = 1.0 if record.success_label else 0.0
        probability = prediction.success_probability
        correct += (probability >= 0.5) == record.success_label
        brier += (probability - target) ** 2
        clipped = min(1.0 - epsilon, max(epsilon, probability))
        log_loss -= target * math.log(clipped) + (1.0 - target) * math.log(1.0 - clipped)
        difference = prediction.value_score - record.value_label
        absolute_error += abs(difference)
        squared_error += difference**2
    count = len(records)
    return Metrics(
        count=count,
        success_accuracy=correct / count,
        success_brier=brier / count,
        success_log_loss=log_loss / count,
        value_mae=absolute_error / count,
        value_rmse=math.sqrt(squared_error / count),
    )
