"""Fixed-seed CPU training with verified, private, hash-bound model bundles."""

from __future__ import annotations

import ctypes
import errno
import hashlib
import io
import json
import math
import os
import random
import re
import shutil
import stat
import sys
import tempfile
from collections.abc import Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Tuple

import torch
from torch import nn

from .baselines import Prediction, constant_baseline, heuristic_baseline, random_baseline
from .metrics import Metrics, evaluate_predictions
from .model import MODEL_SOURCE, BranchValueModel, Vocabulary, collate_records, predict
from .schema import TrajectoryRecord, canonical_records, labelled_records
from .split import SplitConfig, records_for_split, split_by_task_group

MAX_MODEL_BYTES = 64 * 1024 * 1024
MAX_MODEL_JSON_BYTES = 1024 * 1024
MAX_SPLIT_JSON_BYTES = 64 * 1024 * 1024
MAX_METRICS_JSON_BYTES = 1024 * 1024
MAX_MANIFEST_BYTES = 64 * 1024
ARTIFACT_LIMITS = {
    "model.pt": MAX_MODEL_BYTES,
    "model.json": MAX_MODEL_JSON_BYTES,
    "split.json": MAX_SPLIT_JSON_BYTES,
    "training-metrics.json": MAX_METRICS_JSON_BYTES,
}
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
LINUX_AT_FDCWD = -100
LINUX_RENAME_NOREPLACE = 1
DARWIN_RENAME_EXCL = 0x00000004


class ArtifactValidationError(ValueError):
    """A model bundle failed bounded regular-file or integrity validation."""


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
        if (
            isinstance(self.epochs, bool)
            or not isinstance(self.epochs, int)
            or not 1 <= self.epochs <= 1_000
        ):
            raise ValueError("epochs must be between 1 and 1000")
        if (
            isinstance(self.batch_size, bool)
            or not isinstance(self.batch_size, int)
            or not 1 <= self.batch_size <= 4_096
        ):
            raise ValueError("batch_size must be between 1 and 4096")
        if (
            isinstance(self.learning_rate, bool)
            or not isinstance(self.learning_rate, (int, float))
            or not math.isfinite(self.learning_rate)
            or not 0.0 < self.learning_rate <= 1.0
        ):
            raise ValueError("learning_rate must be finite and in (0, 1]")
        if (
            isinstance(self.hidden_size, bool)
            or not isinstance(self.hidden_size, int)
            or not 4 <= self.hidden_size <= 512
        ):
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

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> TrainConfig:
        if not isinstance(value, dict) or set(value) != {
            "seed",
            "epochs",
            "batch_size",
            "learning_rate",
            "hidden_size",
            "encoder",
            "split",
        }:
            raise ArtifactValidationError("artifact training metadata fields are invalid")
        raw_split = value["split"]
        if not isinstance(raw_split, dict):
            raise ArtifactValidationError("artifact training split config is invalid")
        try:
            config = cls(
                seed=value["seed"],
                epochs=value["epochs"],
                batch_size=value["batch_size"],
                learning_rate=value["learning_rate"],
                hidden_size=value["hidden_size"],
                encoder=value["encoder"],
                split=SplitConfig.from_dict(raw_split),
            )
            config.validate()
        except (TypeError, ValueError) as error:
            raise ArtifactValidationError("artifact training metadata is invalid") from error
        if config.as_dict() != value:
            raise ArtifactValidationError("artifact training metadata is not canonical")
        return config


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
    constant: ScoredMetrics

    def as_dict(self) -> Dict[str, Any]:
        return {
            "schema_version": 1,
            "split": self.split,
            "model": self.model.as_dict(),
            "random": self.random.as_dict(),
            "heuristic": self.heuristic.as_dict(),
            "constant": self.constant.as_dict(),
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


@dataclass(frozen=True)
class LoadedModel:
    model: BranchValueModel
    vocabulary: Vocabulary
    metadata: Mapping[str, Any]
    split_document: Mapping[str, Any]


def train_model(
    records: Sequence[TrajectoryRecord], output_dir: os.PathLike[str], config: TrainConfig
) -> TrainingResult:
    config.validate()
    output = Path(output_dir)
    if os.path.lexists(output):
        raise ValueError("output directory already exists; refusing to clobber model artifacts")
    split = split_by_task_group(records, config.split)
    train_records = records_for_split(records, split, "train")
    validation_records = records_for_split(records, split, "validation")
    test_records = records_for_split(records, split, "test")
    vocabulary = Vocabulary.build(train_records)
    constant_success = sum(float(record.success_label) for record in train_records) / len(
        train_records
    )
    constant_value = sum(float(record.value_label) for record in train_records) / len(train_records)
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
        constant_success=constant_success,
        constant_value=constant_value,
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
    weights = io.BytesIO()
    torch.save(model.state_dict(), weights)
    labelled = labelled_records(records)
    payloads = {
        "model.pt": weights.getvalue(),
        "model.json": _json_bytes(
            {
                "format_version": 1,
                "device": "cpu",
                "model_source": MODEL_SOURCE,
                "encoder": config.encoder,
                "hidden_size": config.hidden_size,
                "parameter_count": model.parameter_count(),
                "vocabulary": vocabulary.as_dict(),
                "training": config.as_dict(),
                "constant_baseline": {
                    "success_probability": constant_success,
                    "value_score": constant_value,
                },
            }
        ),
        "split.json": _json_bytes(
            {
                "format_version": 1,
                "dataset_sha256": dataset_fingerprint(labelled),
                "split": split.as_dict(),
            }
        ),
        "training-metrics.json": _json_bytes(result.as_dict()),
    }
    _publish_bundle(output, payloads)
    return result


def evaluate_model(
    records: Sequence[TrajectoryRecord], model_dir: os.PathLike[str], *, split: str = "test"
) -> EvaluationReport:
    if split not in {"train", "validation", "test"}:
        raise ValueError("split must be train, validation, or test")
    loaded = _load_model(model_dir)
    split_document = loaded.split_document
    labelled = labelled_records(records)
    if split_document.get("dataset_sha256") != dataset_fingerprint(labelled):
        raise ValueError("split manifest does not match labelled dataset")
    raw_split = split_document.get("split")
    if not isinstance(raw_split, dict):
        raise ValueError("split manifest is invalid")
    raw_config = raw_split.get("config")
    if not isinstance(raw_config, dict):
        raise ValueError("split manifest config is invalid")
    try:
        config = SplitConfig.from_dict(raw_config)
        recomputed = split_by_task_group(records, config)
    except ValueError as error:
        raise ValueError(f"split manifest cannot be recomputed: {error}") from error
    if raw_split != recomputed.as_dict():
        raise ValueError("split manifest differs from recomputed task-group split")
    selected = records_for_split(labelled, recomputed, split)
    if not selected:
        raise ValueError("selected labelled split is empty")
    constant_success, constant_value = _constant_values(loaded.metadata)
    seed = _training_seed(loaded.metadata)
    return _evaluate_with_model(
        loaded.model,
        loaded.vocabulary,
        selected,
        split=split,
        random_seed=seed,
        constant_success=constant_success,
        constant_value=constant_value,
    )


def score_model(
    records: Sequence[TrajectoryRecord], model_dir: os.PathLike[str]
) -> List[Prediction]:
    """Score labelled or unlabelled records without consulting labels or the training split."""

    if not records:
        raise ValueError("scoring requires at least one trajectory")
    loaded = _load_model(model_dir)
    return predict(loaded.model, records, loaded.vocabulary)


def dataset_fingerprint(records: Sequence[TrajectoryRecord]) -> str:
    return hashlib.sha256(canonical_records(records)).hexdigest()


def _evaluate_with_model(
    model: BranchValueModel,
    vocabulary: Vocabulary,
    records: Sequence[TrajectoryRecord],
    *,
    split: str,
    random_seed: int,
    constant_success: float,
    constant_value: float,
) -> EvaluationReport:
    model_predictions = predict(model, records, vocabulary)
    random_predictions = random_baseline(records, random_seed)
    heuristic_predictions = heuristic_baseline(records)
    constant_predictions = constant_baseline(records, constant_success, constant_value)
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
        constant=ScoredMetrics(
            constant_predictions[0].source,
            evaluate_predictions(records, constant_predictions),
        ),
    )


def _load_model(model_dir: os.PathLike[str]) -> LoadedModel:
    payloads = _load_verified_bundle(Path(model_dir))
    metadata = _decode_object("model.json", payloads["model.json"])
    split_document = _decode_object("split.json", payloads["split.json"])
    if set(metadata) != {
        "format_version",
        "device",
        "model_source",
        "encoder",
        "hidden_size",
        "parameter_count",
        "vocabulary",
        "training",
        "constant_baseline",
    }:
        raise ArtifactValidationError("artifact model.json fields are invalid")
    if (
        metadata.get("format_version") != 1
        or metadata.get("device") != "cpu"
        or metadata.get("model_source") != MODEL_SOURCE
        or metadata.get("encoder") != "gru"
    ):
        raise ArtifactValidationError("artifact model.json contract is unsupported")
    if set(split_document) != {"format_version", "dataset_sha256", "split"}:
        raise ArtifactValidationError("artifact split.json fields are invalid")
    if split_document.get("format_version") != 1 or not _is_sha256(
        split_document.get("dataset_sha256")
    ):
        raise ArtifactValidationError("artifact split.json contract is invalid")
    training_config = _training_config(metadata)
    raw_split = split_document.get("split")
    if not isinstance(raw_split, dict) or set(raw_split) != {
        "schema_version",
        "unit",
        "config",
        "group_assignment",
        "assignment",
    }:
        raise ArtifactValidationError("artifact split manifest fields are invalid")
    if raw_split.get("schema_version") != 1 or raw_split.get("unit") != "task_group_id":
        raise ArtifactValidationError("artifact split manifest contract is invalid")
    raw_split_config = raw_split.get("config")
    if not isinstance(raw_split_config, dict):
        raise ArtifactValidationError("artifact split manifest config is invalid")
    try:
        persisted_split_config = SplitConfig.from_dict(raw_split_config)
    except ValueError as error:
        raise ArtifactValidationError("artifact split manifest config is invalid") from error
    if persisted_split_config.as_dict() != training_config.split.as_dict():
        raise ArtifactValidationError(
            "artifact training split config does not match split manifest"
        )
    _validate_split_mapping_shape(raw_split)
    vocabulary_raw = metadata.get("vocabulary")
    if not isinstance(vocabulary_raw, dict):
        raise ArtifactValidationError("artifact model vocabulary is invalid")
    hidden_size = metadata.get("hidden_size")
    if isinstance(hidden_size, bool) or not isinstance(hidden_size, int):
        raise ArtifactValidationError("artifact model architecture is invalid")
    try:
        vocabulary = Vocabulary.from_dict(vocabulary_raw)
        model = BranchValueModel(vocabulary, hidden_size, "gru").cpu()
        state = torch.load(io.BytesIO(payloads["model.pt"]), map_location="cpu", weights_only=True)
        if not isinstance(state, Mapping):
            raise ValueError("weights are not a state mapping")
        model.load_state_dict(state, strict=True)
    except Exception as error:
        raise ArtifactValidationError(
            "artifact model weights or architecture are invalid"
        ) from error
    parameter_count = metadata.get("parameter_count")
    if (
        isinstance(parameter_count, bool)
        or not isinstance(parameter_count, int)
        or parameter_count != model.parameter_count()
    ):
        raise ArtifactValidationError("artifact model parameter count is invalid")
    _constant_values(metadata)
    return LoadedModel(model, vocabulary, metadata, split_document)


def _load_verified_bundle(root: Path) -> Dict[str, bytes]:
    if os.name != "posix":
        return _load_verified_bundle_by_path(root)
    try:
        flags = (
            os.O_RDONLY
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_DIRECTORY", 0)
            | getattr(os, "O_NOFOLLOW", 0)
        )
        directory_fd = os.open(root, flags)
    except OSError as error:
        raise ArtifactValidationError("artifact model directory is unavailable") from error
    try:
        metadata = os.fstat(directory_fd)
        if not stat.S_ISDIR(metadata.st_mode):
            raise ArtifactValidationError("artifact model path must be a regular directory")
        _require_private_owner_mode(metadata, "model directory", directory=True)
        return _load_verified_payloads(
            lambda name, maximum: _read_bounded_regular_at(directory_fd, name, maximum)
        )
    finally:
        os.close(directory_fd)


def _load_verified_bundle_by_path(root: Path) -> Dict[str, bytes]:
    try:
        root_stat = root.lstat()
    except OSError as error:
        raise ArtifactValidationError("artifact model directory is unavailable") from error
    if not stat.S_ISDIR(root_stat.st_mode) or root.is_symlink():
        raise ArtifactValidationError("artifact model path must be a regular directory")
    return _load_verified_payloads(
        lambda name, maximum: _read_bounded_regular(root / name, maximum)
    )


def _load_verified_payloads(reader) -> Dict[str, bytes]:
    manifest_bytes = reader("manifest.json", MAX_MANIFEST_BYTES)
    manifest = _decode_object("manifest.json", manifest_bytes)
    if set(manifest) != {"format_version", "files"} or manifest.get("format_version") != 1:
        raise ArtifactValidationError("artifact manifest contract is invalid")
    files = manifest.get("files")
    if not isinstance(files, dict) or set(files) != set(ARTIFACT_LIMITS):
        raise ArtifactValidationError("artifact manifest file set is invalid")
    payloads: Dict[str, bytes] = {}
    for name, maximum in ARTIFACT_LIMITS.items():
        entry = files.get(name)
        if not isinstance(entry, dict) or set(entry) != {"sha256", "size"}:
            raise ArtifactValidationError(f"artifact manifest entry {name} is invalid")
        expected_size = entry.get("size")
        expected_hash = entry.get("sha256")
        if (
            isinstance(expected_size, bool)
            or not isinstance(expected_size, int)
            or not 1 <= expected_size <= maximum
            or not _is_sha256(expected_hash)
        ):
            raise ArtifactValidationError(f"artifact manifest entry {name} is invalid")
        payload = reader(name, maximum)
        if len(payload) != expected_size or hashlib.sha256(payload).hexdigest() != expected_hash:
            raise ArtifactValidationError(f"artifact {name} failed manifest verification")
        payloads[name] = payload
    return payloads


def _read_bounded_regular_at(directory_fd: int, name: str, maximum: int) -> bytes:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(name, flags, dir_fd=directory_fd)
    except OSError as error:
        raise ArtifactValidationError(f"artifact {name} is unavailable") from error
    return _read_bounded_descriptor(descriptor, name, maximum, enforce_private=True)


def _read_bounded_regular(path: Path, maximum: int) -> bytes:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise ArtifactValidationError(f"artifact {path.name} is unavailable") from error
    return _read_bounded_descriptor(descriptor, path.name, maximum, enforce_private=False)


def _read_bounded_descriptor(
    descriptor: int, name: str, maximum: int, *, enforce_private: bool
) -> bytes:
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise ArtifactValidationError(f"artifact {name} must be a regular file")
        if enforce_private:
            _require_private_owner_mode(metadata, name, directory=False)
        if not 1 <= metadata.st_size <= maximum:
            raise ArtifactValidationError(f"artifact {name} is empty or exceeds its size bound")
        with os.fdopen(descriptor, "rb") as source:
            descriptor = -1
            payload = source.read(maximum + 1)
        if len(payload) != metadata.st_size or len(payload) > maximum:
            raise ArtifactValidationError(f"artifact {name} changed while being read")
        return payload
    except OSError as error:
        raise ArtifactValidationError(f"artifact {name} cannot be read") from error
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _require_private_owner_mode(metadata: os.stat_result, name: str, *, directory: bool) -> None:
    if metadata.st_uid != os.geteuid():
        raise ArtifactValidationError(f"artifact {name} ownership is unsafe")
    mode = stat.S_IMODE(metadata.st_mode)
    if directory:
        safe = mode in {0o500, 0o700}
    else:
        safe = mode in {0o400, 0o600}
    if not safe:
        raise ArtifactValidationError(f"artifact {name} permissions are unsafe")


def _publish_bundle(output: Path, payloads: Mapping[str, bytes]) -> None:
    if set(payloads) != set(ARTIFACT_LIMITS):
        raise ValueError("model bundle payload set is invalid")
    output.parent.mkdir(parents=True, exist_ok=True)
    if os.path.lexists(output):
        raise ValueError("output directory already exists; refusing to clobber model artifacts")
    stage = Path(tempfile.mkdtemp(prefix=f".{output.name}.", dir=output.parent))
    os.chmod(stage, 0o700)
    published = False
    try:
        manifest_files: Dict[str, Dict[str, Any]] = {}
        for name, maximum in ARTIFACT_LIMITS.items():
            payload = payloads[name]
            if not 1 <= len(payload) <= maximum:
                raise ValueError(f"artifact {name} exceeds its size bound")
            _write_private_new(stage / name, payload)
            manifest_files[name] = {
                "sha256": hashlib.sha256(payload).hexdigest(),
                "size": len(payload),
            }
        manifest = _json_bytes({"format_version": 1, "files": manifest_files})
        if len(manifest) > MAX_MANIFEST_BYTES:
            raise ValueError("artifact manifest exceeds its size bound")
        _write_private_new(stage / "manifest.json", manifest)
        _fsync_directory(stage)
        try:
            _rename_directory_noreplace(stage, output)
        except OSError as error:
            if error.errno not in {errno.EEXIST, errno.ENOTEMPTY}:
                raise
            raise ValueError(
                "output directory already exists; refusing to clobber model artifacts"
            ) from error
        published = True
        _fsync_directory(output.parent)
    finally:
        if not published:
            shutil.rmtree(stage, ignore_errors=True)


def _rename_directory_noreplace(source: Path, destination: Path) -> None:
    """Atomically publish one directory while refusing every existing destination path."""

    libc = ctypes.CDLL(None, use_errno=True)
    source_bytes = os.fsencode(source)
    destination_bytes = os.fsencode(destination)
    if sys.platform.startswith("linux"):
        try:
            rename = libc.renameat2
        except AttributeError as error:
            raise OSError(errno.ENOTSUP, "renameat2 is unavailable") from error
        rename.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_uint,
        ]
        rename.restype = ctypes.c_int
        result = rename(
            LINUX_AT_FDCWD,
            source_bytes,
            LINUX_AT_FDCWD,
            destination_bytes,
            LINUX_RENAME_NOREPLACE,
        )
    elif sys.platform == "darwin":
        try:
            rename = libc.renamex_np
        except AttributeError as error:
            raise OSError(errno.ENOTSUP, "renamex_np is unavailable") from error
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        rename.restype = ctypes.c_int
        result = rename(source_bytes, destination_bytes, DARWIN_RENAME_EXCL)
    elif os.name == "nt":
        os.rename(source, destination)
        return
    else:
        raise OSError(errno.ENOTSUP, "atomic no-replace directory publication is unavailable")
    if result != 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code), os.fspath(destination))


def _write_private_new(path: Path, payload: bytes) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "wb") as output:
            descriptor = -1
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _json_bytes(value: Mapping[str, Any]) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n").encode("utf-8")


def _decode_object(name: str, payload: bytes) -> Dict[str, Any]:
    try:
        value = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ArtifactValidationError(f"artifact {name} is not valid JSON") from error
    if not isinstance(value, dict):
        raise ArtifactValidationError(f"artifact {name} must be a JSON object")
    return value


def _training_config(metadata: Mapping[str, Any]) -> TrainConfig:
    training = metadata.get("training")
    if not isinstance(training, dict):
        raise ArtifactValidationError("artifact training metadata is invalid")
    return TrainConfig.from_dict(training)


def _training_seed(metadata: Mapping[str, Any]) -> int:
    return _training_config(metadata).seed


def _validate_split_mapping_shape(raw_split: Mapping[str, Any]) -> None:
    for name in ("group_assignment", "assignment"):
        mapping = raw_split.get(name)
        if not isinstance(mapping, dict) or not mapping:
            raise ArtifactValidationError(f"artifact split {name} is invalid")
        for identifier, split_name in mapping.items():
            if not _is_sha256(identifier) or split_name not in {"train", "validation", "test"}:
                raise ArtifactValidationError(f"artifact split {name} is invalid")


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _constant_values(metadata: Mapping[str, Any]) -> Tuple[float, float]:
    constant = metadata.get("constant_baseline")
    if not isinstance(constant, dict) or set(constant) != {
        "success_probability",
        "value_score",
    }:
        raise ArtifactValidationError("artifact constant baseline is invalid")
    values = (constant["success_probability"], constant["value_score"])
    if any(
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(float(value))
        or not 0.0 <= float(value) <= 1.0
        for value in values
    ):
        raise ArtifactValidationError("artifact constant baseline is invalid")
    return float(values[0]), float(values[1])


def _is_sha256(value: Any) -> bool:
    return isinstance(value, str) and SHA256_PATTERN.fullmatch(value) is not None


def _configure_determinism(seed: int) -> None:
    random.seed(seed)
    torch.manual_seed(seed)
    torch.use_deterministic_algorithms(True)
