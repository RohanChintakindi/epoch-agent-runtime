"""Strict reader for Epoch's versioned, metadata-only Rust trajectory contract."""

from __future__ import annotations

import json
import math
import os
import re
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

SCHEMA_VERSION = 1
PRIVACY_PROFILE = "metadata_only"
MAX_LINE_BYTES = 256 * 1024
DEFAULT_MAX_RECORDS = 100_000
MAX_EVENTS = 256
MAX_U32 = (1 << 32) - 1
MAX_U64 = (1 << 64) - 1

ACTORS = frozenset({"agent", "supervisor", "tool", "gateway", "operator"})
STATUSES = frozenset({"started", "succeeded", "failed", "denied", "unknown"})
KINDS = frozenset(
    {
        "agent.start",
        "context.update",
        "model.request",
        "model.response",
        "tool.call",
        "tool.result",
        "safe_point",
        "supervisor.run_started",
        "process.started",
        "process.manifest",
        "process.stderr",
        "application.context_restored",
        "other",
    }
)
OPAQUE_ID_PATTERN = re.compile(r"^[0-9a-f]{64}$")


class DatasetValidationError(ValueError):
    """A bounded dataset failed the cross-language trajectory contract."""


@dataclass(frozen=True)
class TrajectoryEvent:
    position: int
    delta_monotonic_ns: int
    actor: str
    kind: str
    status: str
    references_epoch: bool
    has_causal_parent: bool

    def __post_init__(self) -> None:
        _bounded_integer("event.position", self.position, 0, MAX_EVENTS - 1)
        _bounded_integer("event.delta_monotonic_ns", self.delta_monotonic_ns, 0, MAX_U64)
        if self.actor not in ACTORS:
            raise DatasetValidationError("event.actor is not a supported categorical value")
        if self.kind not in KINDS:
            raise DatasetValidationError("event.kind is not in the finite Rust taxonomy")
        if self.status not in STATUSES:
            raise DatasetValidationError("event.status is not a supported categorical value")
        _boolean("event.references_epoch", self.references_epoch)
        _boolean("event.has_causal_parent", self.has_causal_parent)

    def as_dict(self) -> Dict[str, Any]:
        return {
            "position": self.position,
            "delta_monotonic_ns": self.delta_monotonic_ns,
            "actor": self.actor,
            "kind": self.kind,
            "status": self.status,
            "references_epoch": self.references_epoch,
            "has_causal_parent": self.has_causal_parent,
        }

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> TrajectoryEvent:
        _exact_fields(
            "event",
            value,
            {
                "position",
                "delta_monotonic_ns",
                "actor",
                "kind",
                "status",
                "references_epoch",
                "has_causal_parent",
            },
        )
        return cls(
            position=value["position"],
            delta_monotonic_ns=value["delta_monotonic_ns"],
            actor=value["actor"],
            kind=value["kind"],
            status=value["status"],
            references_epoch=value["references_epoch"],
            has_causal_parent=value["has_causal_parent"],
        )


@dataclass(frozen=True)
class TrajectorySummary:
    event_count: int
    duration_monotonic_ns: int
    started_events: int
    succeeded_events: int
    failed_events: int
    denied_events: int
    unknown_events: int

    def __post_init__(self) -> None:
        for name, value in self.as_dict().items():
            _bounded_integer(f"summary.{name}", value, 0, MAX_U64)
        if self.event_count > MAX_EVENTS:
            raise DatasetValidationError(f"summary.event_count cannot exceed {MAX_EVENTS}")

    @classmethod
    def from_events(cls, events: Sequence[TrajectoryEvent]) -> TrajectorySummary:
        counts = {status: 0 for status in STATUSES}
        duration = 0
        for event in events:
            if not isinstance(event, TrajectoryEvent):
                raise DatasetValidationError("summary requires typed trajectory events")
            counts[event.status] += 1
            duration += event.delta_monotonic_ns
            if duration > MAX_U64:
                raise DatasetValidationError("summary.duration_monotonic_ns exceeds u64")
        return cls(
            event_count=len(events),
            duration_monotonic_ns=duration,
            started_events=counts["started"],
            succeeded_events=counts["succeeded"],
            failed_events=counts["failed"],
            denied_events=counts["denied"],
            unknown_events=counts["unknown"],
        )

    def as_dict(self) -> Dict[str, int]:
        return {
            "event_count": self.event_count,
            "duration_monotonic_ns": self.duration_monotonic_ns,
            "started_events": self.started_events,
            "succeeded_events": self.succeeded_events,
            "failed_events": self.failed_events,
            "denied_events": self.denied_events,
            "unknown_events": self.unknown_events,
        }

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> TrajectorySummary:
        fields = {
            "event_count",
            "duration_monotonic_ns",
            "started_events",
            "succeeded_events",
            "failed_events",
            "denied_events",
            "unknown_events",
        }
        _exact_fields("summary", value, fields)
        return cls(**{field: value[field] for field in fields})


@dataclass(frozen=True)
class TrajectoryRecord:
    schema_version: int
    privacy_profile: str
    trajectory_id: str
    task_group_id: str
    session_group_id: str
    candidate_group_id: str
    branch_depth: int
    success_label: Optional[bool]
    value_label: Optional[float]
    events: Tuple[TrajectoryEvent, ...]
    summary: TrajectorySummary

    def __post_init__(self) -> None:
        if self.schema_version != SCHEMA_VERSION:
            raise DatasetValidationError(
                f"schema_version must be {SCHEMA_VERSION}, got {self.schema_version!r}"
            )
        if self.privacy_profile != PRIVACY_PROFILE:
            raise DatasetValidationError(f"privacy_profile must be {PRIVACY_PROFILE!r}")
        for name in (
            "trajectory_id",
            "task_group_id",
            "session_group_id",
            "candidate_group_id",
        ):
            _opaque_id(name, getattr(self, name))
        _bounded_integer("branch_depth", self.branch_depth, 0, MAX_U32)
        if (self.success_label is None) != (self.value_label is None):
            raise DatasetValidationError("success/value label pair must both be present or null")
        if self.success_label is not None:
            _boolean("success_label", self.success_label)
            _bounded_number("value_label", self.value_label, 0.0, 1.0)
        if not isinstance(self.events, tuple) or len(self.events) > MAX_EVENTS:
            raise DatasetValidationError(f"events must contain between 0 and {MAX_EVENTS} entries")
        if any(not isinstance(event, TrajectoryEvent) for event in self.events):
            raise DatasetValidationError("events must contain typed TrajectoryEvent records")
        if [event.position for event in self.events] != list(range(len(self.events))):
            raise DatasetValidationError("event positions must be contiguous from zero")
        if self.events and self.events[0].delta_monotonic_ns != 0:
            raise DatasetValidationError("first event delta_monotonic_ns must be zero")
        if not isinstance(self.summary, TrajectorySummary):
            raise DatasetValidationError("summary must be a typed TrajectorySummary")
        if self.summary != TrajectorySummary.from_events(self.events):
            raise DatasetValidationError("summary must exactly match events")

    @property
    def is_labelled(self) -> bool:
        return self.success_label is not None

    def as_dict(self) -> Dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "privacy_profile": self.privacy_profile,
            "trajectory_id": self.trajectory_id,
            "task_group_id": self.task_group_id,
            "session_group_id": self.session_group_id,
            "candidate_group_id": self.candidate_group_id,
            "branch_depth": self.branch_depth,
            "success_label": self.success_label,
            "value_label": self.value_label,
            "events": [event.as_dict() for event in self.events],
            "summary": self.summary.as_dict(),
        }

    def with_labels(self, *, success: Optional[bool], value: Optional[float]) -> TrajectoryRecord:
        return replace(self, success_label=success, value_label=value)

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> TrajectoryRecord:
        _exact_fields(
            "trajectory",
            value,
            {
                "schema_version",
                "privacy_profile",
                "trajectory_id",
                "task_group_id",
                "session_group_id",
                "candidate_group_id",
                "branch_depth",
                "success_label",
                "value_label",
                "events",
                "summary",
            },
        )
        raw_events = value["events"]
        if not isinstance(raw_events, list):
            raise DatasetValidationError("events must be a JSON array")
        if len(raw_events) > MAX_EVENTS:
            raise DatasetValidationError(f"events cannot contain more than {MAX_EVENTS} entries")
        raw_summary = value["summary"]
        if not isinstance(raw_summary, dict):
            raise DatasetValidationError("summary must be a JSON object")
        return cls(
            schema_version=value["schema_version"],
            privacy_profile=value["privacy_profile"],
            trajectory_id=value["trajectory_id"],
            task_group_id=value["task_group_id"],
            session_group_id=value["session_group_id"],
            candidate_group_id=value["candidate_group_id"],
            branch_depth=value["branch_depth"],
            success_label=value["success_label"],
            value_label=value["value_label"],
            events=tuple(TrajectoryEvent.from_dict(event) for event in raw_events),
            summary=TrajectorySummary.from_dict(raw_summary),
        )


def load_jsonl(
    path: os.PathLike[str], *, max_records: int = DEFAULT_MAX_RECORDS
) -> List[TrajectoryRecord]:
    """Read JSONL with a bounded readline so an unterminated line cannot allocate unboundedly."""

    if isinstance(max_records, bool) or not isinstance(max_records, int) or max_records < 1:
        raise DatasetValidationError("maximum record count must be a positive integer")
    records: List[TrajectoryRecord] = []
    seen_trajectories = set()
    candidate_tasks: Dict[str, str] = {}
    with Path(path).open("rb") as source:
        line_number = 0
        while True:
            encoded = source.readline(MAX_LINE_BYTES + 1)
            if encoded == b"":
                break
            line_number += 1
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
            previous_task = candidate_tasks.setdefault(
                record.candidate_group_id, record.task_group_id
            )
            if previous_task != record.task_group_id:
                raise DatasetValidationError(
                    f"line {line_number}: candidate_group_id appears in multiple task groups"
                )
            seen_trajectories.add(record.trajectory_id)
            records.append(record)
    if not records:
        raise DatasetValidationError("dataset must contain at least one record")
    return records


def write_jsonl(path: os.PathLike[str], records: Iterable[TrajectoryRecord]) -> None:
    """Create a canonical private JSONL file and refuse to replace any existing path."""

    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    materialized = list(records)
    _validate_dataset_identities(materialized)
    descriptor: Optional[int] = None
    created = False
    try:
        try:
            descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            created = True
        except FileExistsError as error:
            raise DatasetValidationError(f"output already exists: {destination}") from error
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            descriptor = None
            for record in materialized:
                output.write(
                    json.dumps(
                        record.as_dict(),
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


def canonical_records(records: Sequence[TrajectoryRecord]) -> bytes:
    ordered = sorted(records, key=lambda record: record.trajectory_id)
    return b"".join(
        (
            json.dumps(record.as_dict(), sort_keys=True, separators=(",", ":"), allow_nan=False)
            + "\n"
        ).encode("utf-8")
        for record in ordered
    )


def labelled_records(records: Iterable[TrajectoryRecord]) -> Tuple[TrajectoryRecord, ...]:
    return tuple(record for record in records if record.is_labelled)


def _validate_dataset_identities(records: Sequence[TrajectoryRecord]) -> None:
    if not records:
        raise DatasetValidationError("dataset must contain at least one record")
    trajectories = set()
    candidate_tasks: Dict[str, str] = {}
    for record in records:
        if not isinstance(record, TrajectoryRecord):
            raise DatasetValidationError("writer accepts only typed TrajectoryRecord values")
        if record.trajectory_id in trajectories:
            raise DatasetValidationError(f"duplicate trajectory_id {record.trajectory_id}")
        trajectories.add(record.trajectory_id)
        previous = candidate_tasks.setdefault(record.candidate_group_id, record.task_group_id)
        if previous != record.task_group_id:
            raise DatasetValidationError("candidate_group_id appears in multiple task groups")


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


def _opaque_id(name: str, value: Any) -> None:
    if not isinstance(value, str) or not OPAQUE_ID_PATTERN.fullmatch(value):
        raise DatasetValidationError(f"{name} must be exactly 64 lowercase hexadecimal characters")


def _boolean(name: str, value: Any) -> None:
    if not isinstance(value, bool):
        raise DatasetValidationError(f"{name} must be a Boolean")


def _bounded_integer(name: str, value: Any, minimum: int, maximum: int) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise DatasetValidationError(f"{name} must be an integer in [{minimum}, {maximum}]")


def _bounded_number(name: str, value: Any, minimum: float, maximum: float) -> None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise DatasetValidationError(f"{name} must be a finite number in [{minimum}, {maximum}]")
    converted = float(value)
    if not math.isfinite(converted) or not minimum <= converted <= maximum:
        raise DatasetValidationError(f"{name} must be a finite number in [{minimum}, {maximum}]")
