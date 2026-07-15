"""Fixed-seed CPU training, artifact persistence, and baseline evaluation."""

from __future__ import annotations

import hashlib
import json
import math
import os
import random
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List

import torch
from torch import nn

from .baselines import heuristic_baseline, random_baseline
from .metrics import Metrics, evaluate_predictions
from .model import BranchValueModel, Vocabulary, collate_records, predict
from .schema import TrajectoryRecord, canonical_records
from .split import SplitConfig, records_for_split, split_by_task_group


@dataclass(frozen=True)
class TrainConfig:
    seed: int = 23
    epochs: int = 10
    batch_size: int = 32
    learning_rate: float = 0.003
    hidden_size: int = 32
    encoder: str = "gru"
    split: SplitConfig = field(default_factory=SplitConfig)

    def validate(self) -> None:
        if isinstance(self.seed, bool) or not isinstance(self.seed, int) or self.seed < 0:
            raise ValueError("training seed must be a nonnegative integer")
        if not 1 <= self.epochs <= 1_000:
            raise ValueError("epochs must be between 1 and 1000")
        if not 1 <= self.batch_size <= 4_096:
            raise ValueError("batch_size must be between 1 and 4096")
        if not math.isfinite(self.learning_rate) or not 0.0 < self.learning_rate <= 1.0:
            raise ValueError("learning_rate must be finite and in (0, 1]")
        if not 4 <= self.hidden_size <= 512:
            raise ValueError("hidden_size must be between 4 and 512")
        if self.encoder != "gru":
            raise ValueError("the first experiment registers only the GRU encoder")
        self.split.validate()

    def as_dict(self) -> Dict[str, Any]:
        return {
            "seed": self.seed,
            "epochs": self.epochs,
            "batch_size": self.batch_size,
            "learning_rate": self.learning_rate,
            "hidden_size": self.hidden_size,
            "encoder": self.encoder,
            "split": self.split.as_dict(),
        }


@dataclass(frozen=True)
class ScoredMetrics:
    source: str
    metrics: Metrics

    def as_dict(self) -> Dict[str, Any]:
        return {"source": self.source, **self.metrics.as_dict()}


@dataclass(frozen=True)
class EvaluationReport:
    split: str
    model: ScoredMetrics
    random: ScoredMetrics
    heuristic: ScoredMetrics

    def as_dict(self) -> Dict[str, Any]:
        return {
            "schema_version": 1,
            "split": self.split,
            "model": self.model.as_dict(),
            "random": self.random.as_dict(),
            "heuristic": self.heuristic.as_dict(),
        }


@dataclass(frozen=True)
class TrainingResult:
    seed: int
    epochs: int
    train_records: int
    validation_records: int
    test_records: int
    parameter_count: int
    final_train_loss: float
    validation: EvaluationReport

    def as_dict(self) -> Dict[str, Any]:
        return {
            "schema_version": 1,
            "seed": self.seed,
            "epochs": self.epochs,
            "train_records": self.train_records,
            "validation_records": self.validation_records,
            "test_records": self.test_records,
            "parameter_count": self.parameter_count,
            "final_train_loss": self.final_train_loss,
            "validation": self.validation.as_dict(),
        }


def train_model(
    records: Sequence[TrajectoryRecord], output_dir: os.PathLike[str], config: TrainConfig
) -> TrainingResult:
    config.validate()
    output = Path(output_dir)
    if output.exists():
        raise ValueError("output directory already exists; refusing to clobber model artifacts")
    split = split_by_task_group(records, config.split)
    train_records = records_for_split(records, split, "train")
    validation_records = records_for_split(records, split, "validation")
    test_records = records_for_split(records, split, "test")
    vocabulary = Vocabulary.build(train_records)
    _configure_determinism(config.seed)
    model = BranchValueModel(vocabulary, config.hidden_size, config.encoder).cpu()
    optimizer = torch.optim.Adam(model.parameters(), lr=config.learning_rate)
    success_loss = nn.BCEWithLogitsLoss()
    value_loss = nn.MSELoss()
    final_train_loss = 0.0
    ordered_train = sorted(train_records, key=lambda record: record.trajectory_id)
    for epoch in range(config.epochs):
        epoch_records = list(ordered_train)
        random.Random(config.seed + epoch).shuffle(epoch_records)
        model.train()
        loss_sum = 0.0
        examples = 0
        for start in range(0, len(epoch_records), config.batch_size):
            chunk = epoch_records[start : start + config.batch_size]
            batch = collate_records(chunk, vocabulary, torch.device("cpu"))
            optimizer.zero_grad(set_to_none=True)
            success_logits, value_logits = model(batch)
            loss = success_loss(success_logits, batch.success_targets) + value_loss(
                torch.sigmoid(value_logits), batch.value_targets
            )
            loss.backward()
            optimizer.step()
            loss_sum += float(loss.detach()) * len(chunk)
            examples += len(chunk)
        final_train_loss = loss_sum / examples
    validation = _evaluate_with_model(
        model,
        vocabulary,
        validation_records,
        split="validation",
        random_seed=config.seed,
    )
    result = TrainingResult(
        seed=config.seed,
        epochs=config.epochs,
        train_records=len(train_records),
        validation_records=len(validation_records),
        test_records=len(test_records),
        parameter_count=model.parameter_count(),
        final_train_loss=final_train_loss,
        validation=validation,
    )
    output.mkdir(parents=True)
    torch.save(model.state_dict(), output / "model.pt")
    _write_json(
        output / "model.json",
        {
            "format_version": 1,
            "device": "cpu",
            "model_source": "sequence_encoder_v1",
            "encoder": config.encoder,
            "hidden_size": config.hidden_size,
            "parameter_count": model.parameter_count(),
            "vocabulary": vocabulary.as_dict(),
            "training": config.as_dict(),
        },
    )
    _write_json(
        output / "split.json",
        {
            "format_version": 1,
            "dataset_sha256": dataset_fingerprint(records),
            "split": split.as_dict(),
        },
    )
    _write_json(output / "training-metrics.json", result.as_dict())
    return result


def evaluate_model(
    records: Sequence[TrajectoryRecord], model_dir: os.PathLike[str], *, split: str = "test"
) -> EvaluationReport:
    if split not in {"train", "validation", "test"}:
        raise ValueError("split must be train, validation, or test")
    root = Path(model_dir)
    metadata = _read_object(root / "model.json")
    split_manifest = _read_object(root / "split.json")
    if metadata.get("format_version") != 1 or split_manifest.get("format_version") != 1:
        raise ValueError("model artifact format is unsupported")
    if split_manifest.get("dataset_sha256") != dataset_fingerprint(records):
        raise ValueError("split manifest does not match dataset")
    raw_split = split_manifest.get("split")
    if not isinstance(raw_split, dict):
        raise ValueError("split manifest is invalid")
    assignment = raw_split.get("assignment")
    if not isinstance(assignment, dict) or set(assignment) != {
        record.trajectory_id for record in records
    }:
        raise ValueError("split manifest does not match dataset identities")
    group_splits: Dict[str, str] = {}
    selected: List[TrajectoryRecord] = []
    for record in records:
        assigned = assignment.get(record.trajectory_id)
        if assigned not in {"train", "validation", "test"}:
            raise ValueError("split manifest contains an invalid assignment")
        previous = group_splits.setdefault(record.task_group_id, assigned)
        if previous != assigned:
            raise ValueError("split manifest leaks one task group across partitions")
        if assigned == split:
            selected.append(record)
    if not selected:
        raise ValueError("selected split is empty")
    vocabulary_raw = metadata.get("vocabulary")
    if not isinstance(vocabulary_raw, dict):
        raise ValueError("model vocabulary is invalid")
    vocabulary = Vocabulary.from_dict(vocabulary_raw)
    hidden_size = metadata.get("hidden_size")
    encoder = metadata.get("encoder")
    if (
        isinstance(hidden_size, bool)
        or not isinstance(hidden_size, int)
        or not isinstance(encoder, str)
    ):
        raise ValueError("model architecture metadata is invalid")
    model = BranchValueModel(vocabulary, hidden_size, encoder).cpu()
    try:
        state = torch.load(root / "model.pt", map_location="cpu", weights_only=True)
    except (OSError, RuntimeError) as error:
        raise ValueError("model weights are unavailable or invalid") from error
    if not isinstance(state, Mapping):
        raise ValueError("model weights are invalid")
    model.load_state_dict(state, strict=True)
    training = metadata.get("training")
    if not isinstance(training, dict) or not isinstance(training.get("seed"), int):
        raise ValueError("training metadata is invalid")
    return _evaluate_with_model(
        model,
        vocabulary,
        selected,
        split=split,
        random_seed=training["seed"],
    )


def dataset_fingerprint(records: Sequence[TrajectoryRecord]) -> str:
    return hashlib.sha256(canonical_records(records)).hexdigest()


def _evaluate_with_model(
    model: BranchValueModel,
    vocabulary: Vocabulary,
    records: Sequence[TrajectoryRecord],
    *,
    split: str,
    random_seed: int,
) -> EvaluationReport:
    model_predictions = predict(model, records, vocabulary)
    random_predictions = random_baseline(records, random_seed)
    heuristic_predictions = heuristic_baseline(records)
    return EvaluationReport(
        split=split,
        model=ScoredMetrics(
            model_predictions[0].source, evaluate_predictions(records, model_predictions)
        ),
        random=ScoredMetrics(
            random_predictions[0].source, evaluate_predictions(records, random_predictions)
        ),
        heuristic=ScoredMetrics(
            heuristic_predictions[0].source,
            evaluate_predictions(records, heuristic_predictions),
        ),
    )


def _configure_determinism(seed: int) -> None:
    random.seed(seed)
    torch.manual_seed(seed)
    torch.use_deterministic_algorithms(True)


def _write_json(path: Path, value: Mapping[str, Any]) -> None:
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n",
        encoding="utf-8",
    )


def _read_object(path: Path) -> Dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"artifact {path.name} is unavailable or invalid") from error
    if not isinstance(value, dict):
        raise ValueError(f"artifact {path.name} must be a JSON object")
    return value
