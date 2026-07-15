"""Strict, text-free trajectory schema and bounded JSONL I/O."""

from __future__ import annotations

import json
import math
import os
import re
import tempfile
import uuid
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

SCHEMA_VERSION = 1
MAX_LINE_BYTES = 256 * 1024
DEFAULT_MAX_RECORDS = 100_000
MAX_STEPS = 256
MAX_DURATION_MS = 3_600_000.0
MAX_TOKEN_COUNT = 1_000_000
MAX_EFFECT_COUNT = 1_000
MAX_CAPABILITY_COUNT = 1_000

ACTORS = frozenset({"agent", "supervisor", "tool", "gateway", "operator"})
STATUSES = frozenset({"started", "succeeded", "failed", "denied", "unknown"})
KIND_PATTERN = re.compile(r"^[a-z0-9][a-z0-9._]{0,63}$")
TASK_GROUP_PATTERN = re.compile(r"^tg_[0-9a-f]{16,64}$")


class DatasetValidationError(ValueError):
    """A bounded dataset failed the versioned schema contract."""


@dataclass(frozen=True)
class Step:
    sequence: int
    actor: str
    kind: str
    status: str
    duration_ms: float
    token_count: int
    effect_count: int
    capability_count: int

    def __post_init__(self) -> None:
        _bounded_integer("step.sequence", self.sequence, 0, MAX_STEPS - 1)
        if self.actor not in ACTORS:
            raise DatasetValidationError("step.actor is not a supported categorical value")
        if not isinstance(self.kind, str) or not KIND_PATTERN.fullmatch(self.kind):
            raise DatasetValidationError("step.kind must be a normalized categorical token")
        if self.status not in STATUSES:
            raise DatasetValidationError("step.status is not a supported categorical value")
        _bounded_number("step.duration_ms", self.duration_ms, 0.0, MAX_DURATION_MS)
        _bounded_integer("step.token_count", self.token_count, 0, MAX_TOKEN_COUNT)
        _bounded_integer("step.effect_count", self.effect_count, 0, MAX_EFFECT_COUNT)
        _bounded_integer(
            "step.capability_count",
            self.capability_count,
            0,
            MAX_CAPABILITY_COUNT,
        )

    def as_dict(self) -> Dict[str, Any]:
        return {
            "sequence": self.sequence,
            "actor": self.actor,
            "kind": self.kind,
            "status": self.status,
            "duration_ms": self.duration_ms,
            "token_count": self.token_count,
            "effect_count": self.effect_count,
            "capability_count": self.capability_count,
        }

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> Step:
        _exact_fields(
            "step",
            value,
            {
                "sequence",
                "actor",
                "kind",
                "status",
                "duration_ms",
                "token_count",
                "effect_count",
                "capability_count",
            },
        )
        return cls(
            sequence=value["sequence"],
            actor=value["actor"],
            kind=value["kind"],
            status=value["status"],
            duration_ms=value["duration_ms"],
            token_count=value["token_count"],
            effect_count=value["effect_count"],
            capability_count=value["capability_count"],
        )


@dataclass(frozen=True)
class Label:
    success: bool
    value: float

    def __post_init__(self) -> None:
        if not isinstance(self.success, bool):
            raise DatasetValidationError("label.success must be a Boolean")
        _bounded_number("label.value", self.value, 0.0, 1.0)

    def as_dict(self) -> Dict[str, Any]:
        return {"success": self.success, "value": self.value}

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> Label:
        _exact_fields("label", value, {"success", "value"})
        return cls(success=value["success"], value=value["value"])


@dataclass(frozen=True)
class TrajectoryRecord:
    schema_version: int
    trajectory_id: str
    task_group_id: str
    branch_id: str
    parent_branch_id: Optional[str]
    steps: Tuple[Step, ...]
    label: Label

    def __post_init__(self) -> None:
        if self.schema_version != SCHEMA_VERSION:
            raise DatasetValidationError(
                f"schema_version must be {SCHEMA_VERSION}, got {self.schema_version!r}"
            )
        _uuid4("trajectory_id", self.trajectory_id)
        if not isinstance(self.task_group_id, str) or not TASK_GROUP_PATTERN.fullmatch(
            self.task_group_id
        ):
            raise DatasetValidationError("task_group_id must be an opaque tg_ hexadecimal digest")
        _uuid4("branch_id", self.branch_id)
        if self.parent_branch_id is not None:
            _uuid4("parent_branch_id", self.parent_branch_id)
            if self.parent_branch_id == self.branch_id:
                raise DatasetValidationError("parent_branch_id cannot equal branch_id")
        if not isinstance(self.steps, tuple) or not 1 <= len(self.steps) <= MAX_STEPS:
            raise DatasetValidationError(f"steps must contain between 1 and {MAX_STEPS} entries")
        if any(not isinstance(step, Step) for step in self.steps):
            raise DatasetValidationError("steps must contain typed Step records")
        if [step.sequence for step in self.steps] != list(range(len(self.steps))):
            raise DatasetValidationError("step sequences must be contiguous from zero")
        if not isinstance(self.label, Label):
            raise DatasetValidationError("label must be a typed Label")

    def as_dict(self) -> Dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "trajectory_id": self.trajectory_id,
            "task_group_id": self.task_group_id,
            "branch_id": self.branch_id,
            "parent_branch_id": self.parent_branch_id,
            "steps": [step.as_dict() for step in self.steps],
            "label": self.label.as_dict(),
        }

    def with_label(self, *, success: bool, value: float) -> TrajectoryRecord:
        return replace(self, label=Label(success=success, value=value))

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> TrajectoryRecord:
        _exact_fields(
            "trajectory",
            value,
            {
                "schema_version",
                "trajectory_id",
                "task_group_id",
                "branch_id",
                "parent_branch_id",
                "steps",
                "label",
            },
        )
        raw_steps = value["steps"]
        if not isinstance(raw_steps, list):
            raise DatasetValidationError("steps must be a JSON array")
        raw_label = value["label"]
        if not isinstance(raw_label, dict):
            raise DatasetValidationError("label must be a JSON object")
        return cls(
            schema_version=value["schema_version"],
            trajectory_id=value["trajectory_id"],
            task_group_id=value["task_group_id"],
            branch_id=value["branch_id"],
            parent_branch_id=value["parent_branch_id"],
            steps=tuple(Step.from_dict(step) for step in raw_steps),
            label=Label.from_dict(raw_label),
        )


def load_jsonl(
    path: os.PathLike[str], *, max_records: int = DEFAULT_MAX_RECORDS
) -> List[TrajectoryRecord]:
    """Load a bounded UTF-8 JSONL dataset with duplicate-key and identity checks."""

    if isinstance(max_records, bool) or not isinstance(max_records, int) or max_records < 1:
        raise DatasetValidationError("maximum record count must be a positive integer")
    records: List[TrajectoryRecord] = []
    seen_trajectories = set()
    with Path(path).open("rb") as source:
        for line_number, encoded in enumerate(source, start=1):
            if len(encoded) > MAX_LINE_BYTES:
                raise DatasetValidationError(
                    f"line {line_number}: record exceeds {MAX_LINE_BYTES} byte limit"
                )
            if len(records) >= max_records:
                raise DatasetValidationError(
                    f"line {line_number}: maximum record count {max_records} exceeded"
                )
            if not encoded.endswith(b"\n"):
                raise DatasetValidationError(f"line {line_number}: JSONL record lacks newline")
            try:
                text = encoded.decode("utf-8")
            except UnicodeDecodeError as error:
                raise DatasetValidationError(f"line {line_number}: invalid UTF-8") from error
            if not text.strip():
                raise DatasetValidationError(f"line {line_number}: blank JSONL records are invalid")
            try:
                decoded = json.loads(text, object_pairs_hook=_unique_object)
            except (json.JSONDecodeError, _DuplicateKey) as error:
                raise DatasetValidationError(f"line {line_number}: {error}") from error
            if not isinstance(decoded, dict):
                raise DatasetValidationError(f"line {line_number}: record must be a JSON object")
            try:
                record = TrajectoryRecord.from_dict(decoded)
            except (DatasetValidationError, KeyError, TypeError) as error:
                raise DatasetValidationError(f"line {line_number}: {error}") from error
            if record.trajectory_id in seen_trajectories:
                raise DatasetValidationError(
                    f"line {line_number}: duplicate trajectory_id {record.trajectory_id}"
                )
            seen_trajectories.add(record.trajectory_id)
            records.append(record)
    if not records:
        raise DatasetValidationError("dataset must contain at least one record")
    return records


def write_jsonl(path: os.PathLike[str], records: Iterable[TrajectoryRecord]) -> None:
    """Atomically write canonical compact JSONL after revalidating identities."""

    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    materialized = list(records)
    if not materialized:
        raise DatasetValidationError("dataset must contain at least one record")
    seen = set()
    for record in materialized:
        if not isinstance(record, TrajectoryRecord):
            raise DatasetValidationError("writer accepts only typed TrajectoryRecord values")
        if record.trajectory_id in seen:
            raise DatasetValidationError(f"duplicate trajectory_id {record.trajectory_id}")
        seen.add(record.trajectory_id)
    temporary_name: Optional[str] = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=destination.parent,
            prefix=f".{destination.name}.",
            suffix=".tmp",
            delete=False,
        ) as temporary:
            temporary_name = temporary.name
            for record in materialized:
                temporary.write(
                    json.dumps(record.as_dict(), sort_keys=True, separators=(",", ":")) + "\n"
                )
            temporary.flush()
            os.fsync(temporary.fileno())
        os.replace(temporary_name, destination)
        temporary_name = None
    finally:
        if temporary_name is not None:
            Path(temporary_name).unlink(missing_ok=True)


def canonical_records(records: Sequence[TrajectoryRecord]) -> bytes:
    ordered = sorted(records, key=lambda record: record.trajectory_id)
    return b"".join(
        (json.dumps(record.as_dict(), sort_keys=True, separators=(",", ":")) + "\n").encode("utf-8")
        for record in ordered
    )


class _DuplicateKey(ValueError):
    pass


def _unique_object(pairs: Sequence[Tuple[str, Any]]) -> Dict[str, Any]:
    value: Dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise _DuplicateKey(f"duplicate JSON key {key!r}")
        value[key] = item
    return value


def _exact_fields(name: str, value: Mapping[str, Any], expected: set) -> None:
    if not isinstance(value, dict):
        raise DatasetValidationError(f"{name} must be a JSON object")
    actual = set(value)
    unknown = actual - expected
    missing = expected - actual
    if unknown:
        raise DatasetValidationError(f"{name} has unknown field {sorted(unknown)[0]!r}")
    if missing:
        raise DatasetValidationError(f"{name} is missing field {sorted(missing)[0]!r}")


def _uuid4(name: str, value: Any) -> None:
    if not isinstance(value, str):
        raise DatasetValidationError(f"{name} must be a canonical UUIDv4 string")
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError) as error:
        raise DatasetValidationError(f"{name} must be a canonical UUIDv4 string") from error
    if str(parsed) != value or parsed.version != 4:
        raise DatasetValidationError(f"{name} must be a canonical UUIDv4 string")


def _bounded_integer(name: str, value: Any, minimum: int, maximum: int) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise DatasetValidationError(f"{name} must be an integer in [{minimum}, {maximum}]")


def _bounded_number(name: str, value: Any, minimum: float, maximum: float) -> None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise DatasetValidationError(f"{name} must be a finite number in [{minimum}, {maximum}]")
    converted = float(value)
    if not math.isfinite(converted) or not minimum <= converted <= maximum:
        raise DatasetValidationError(f"{name} must be a finite number in [{minimum}, {maximum}]")
