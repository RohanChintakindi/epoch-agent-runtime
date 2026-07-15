"""Small pluggable CPU sequence encoder with advisory success/value heads."""

from __future__ import annotations

import math
from abc import ABC, abstractmethod
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any, Dict, List, Tuple

import torch
from torch import Tensor, nn
from torch.nn.utils.rnn import pack_padded_sequence

from .baselines import Prediction
from .schema import (
    ACTORS,
    KINDS,
    MAX_U64,
    STATUSES,
    TrajectoryRecord,
)

PAD = "<pad>"
MODEL_SOURCE = "sequence_encoder_v1"


@dataclass(frozen=True)
class Vocabulary:
    actors: Tuple[str, ...]
    statuses: Tuple[str, ...]
    kinds: Tuple[str, ...]

    @classmethod
    def build(cls, records: Sequence[TrajectoryRecord]) -> Vocabulary:
        if not records:
            raise ValueError("cannot build a vocabulary without trajectories")
        return cls(
            actors=(PAD, *sorted(ACTORS)),
            statuses=(PAD, *sorted(STATUSES)),
            kinds=(PAD, *sorted(KINDS)),
        )

    def __post_init__(self) -> None:
        if self.actors != (PAD, *sorted(ACTORS)):
            raise ValueError("actor vocabulary is invalid")
        if self.statuses != (PAD, *sorted(STATUSES)):
            raise ValueError("status vocabulary is invalid")
        if self.kinds != (PAD, *sorted(KINDS)):
            raise ValueError("kind vocabulary is invalid")

    def as_dict(self) -> Dict[str, Any]:
        return {
            "format_version": 1,
            "actors": list(self.actors),
            "statuses": list(self.statuses),
            "kinds": list(self.kinds),
        }

    @classmethod
    def from_dict(cls, value: Mapping[str, Any]) -> Vocabulary:
        if set(value) != {"format_version", "actors", "statuses", "kinds"}:
            raise ValueError("vocabulary fields are invalid")
        if value["format_version"] != 1:
            raise ValueError("vocabulary format version is unsupported")
        for name in ("actors", "statuses", "kinds"):
            if not isinstance(value[name], list) or not all(
                isinstance(item, str) for item in value[name]
            ):
                raise ValueError(f"vocabulary {name} must be a string array")
        return cls(
            actors=tuple(value["actors"]),
            statuses=tuple(value["statuses"]),
            kinds=tuple(value["kinds"]),
        )


@dataclass(frozen=True)
class SequenceBatch:
    actor_ids: Tensor
    status_ids: Tensor
    kind_ids: Tensor
    numeric: Tensor
    lengths: Tensor
    success_targets: Tensor
    value_targets: Tensor
    trajectory_ids: Tuple[str, ...]


class SequenceEncoder(nn.Module, ABC):
    """Pluggable boundary for controlled encoder comparisons."""

    output_size: int

    @abstractmethod
    def forward(self, sequence: Tensor, lengths: Tensor) -> Tensor:
        """Encode one padded sequence batch into one vector per trajectory."""


class GruSequenceEncoder(SequenceEncoder):
    """Single-layer GRU for small initial datasets and short trajectories."""

    def __init__(self, input_size: int, hidden_size: int) -> None:
        super().__init__()
        self.output_size = hidden_size
        self.gru = nn.GRU(input_size=input_size, hidden_size=hidden_size, batch_first=True)

    def forward(self, sequence: Tensor, lengths: Tensor) -> Tensor:
        packed = pack_padded_sequence(
            sequence,
            lengths.cpu(),
            batch_first=True,
            enforce_sorted=False,
        )
        _, final = self.gru(packed)
        return final[-1]


ENCODER_NAMES = ("gru",)


def build_encoder(name: str, input_size: int, hidden_size: int) -> SequenceEncoder:
    if name == "gru":
        return GruSequenceEncoder(input_size, hidden_size)
    raise ValueError(f"unsupported encoder {name!r}; available encoders: {ENCODER_NAMES}")


class BranchValueModel(nn.Module):
    """Categorical/numeric sequence model returning scores, never runtime authority."""

    def __init__(
        self,
        vocabulary: Vocabulary,
        hidden_size: int = 32,
        encoder_name: str = "gru",
    ) -> None:
        super().__init__()
        if not 4 <= hidden_size <= 512:
            raise ValueError("hidden_size must be between 4 and 512")
        self.vocabulary = vocabulary
        self.hidden_size = hidden_size
        self.encoder_name = encoder_name
        self.actor_embedding = nn.Embedding(len(vocabulary.actors), 8, padding_idx=0)
        self.status_embedding = nn.Embedding(len(vocabulary.statuses), 8, padding_idx=0)
        self.kind_embedding = nn.Embedding(len(vocabulary.kinds), 16, padding_idx=0)
        self.encoder = build_encoder(encoder_name, 8 + 8 + 16 + 3, hidden_size)
        self.success_head = nn.Linear(self.encoder.output_size, 1)
        self.value_head = nn.Linear(self.encoder.output_size, 1)

    def forward(self, batch: SequenceBatch) -> Tuple[Tensor, Tensor]:
        sequence = torch.cat(
            (
                self.actor_embedding(batch.actor_ids),
                self.status_embedding(batch.status_ids),
                self.kind_embedding(batch.kind_ids),
                batch.numeric,
            ),
            dim=-1,
        )
        encoded = self.encoder(sequence, batch.lengths)
        return self.success_head(encoded).squeeze(-1), self.value_head(encoded).squeeze(-1)

    def parameter_count(self) -> int:
        return sum(parameter.numel() for parameter in self.parameters())


def collate_records(
    records: Sequence[TrajectoryRecord], vocabulary: Vocabulary, device: torch.device
) -> SequenceBatch:
    if not records:
        raise ValueError("cannot collate an empty batch")
    actor_index = {value: index for index, value in enumerate(vocabulary.actors)}
    status_index = {value: index for index, value in enumerate(vocabulary.statuses)}
    kind_index = {value: index for index, value in enumerate(vocabulary.kinds)}
    maximum_events = max(1, max(len(record.events) for record in records))
    actor_ids = torch.zeros((len(records), maximum_events), dtype=torch.long, device=device)
    status_ids = torch.zeros_like(actor_ids)
    kind_ids = torch.zeros_like(actor_ids)
    numeric = torch.zeros((len(records), maximum_events, 3), dtype=torch.float32, device=device)
    lengths = torch.tensor(
        [max(1, len(record.events)) for record in records], dtype=torch.long, device=device
    )
    for row, record in enumerate(records):
        for column, event in enumerate(record.events):
            actor_ids[row, column] = actor_index[event.actor]
            status_ids[row, column] = status_index[event.status]
            kind_ids[row, column] = kind_index[event.kind]
            numeric[row, column] = torch.tensor(
                (
                    _log_scale(event.delta_monotonic_ns, MAX_U64),
                    float(event.references_epoch),
                    float(event.has_causal_parent),
                ),
                dtype=torch.float32,
                device=device,
            )
    return SequenceBatch(
        actor_ids=actor_ids,
        status_ids=status_ids,
        kind_ids=kind_ids,
        numeric=numeric,
        lengths=lengths,
        success_targets=torch.tensor(
            [
                float(record.success_label) if record.success_label is not None else float("nan")
                for record in records
            ],
            dtype=torch.float32,
            device=device,
        ),
        value_targets=torch.tensor(
            [
                record.value_label if record.value_label is not None else float("nan")
                for record in records
            ],
            dtype=torch.float32,
            device=device,
        ),
        trajectory_ids=tuple(record.trajectory_id for record in records),
    )


def predict(
    model: BranchValueModel,
    records: Sequence[TrajectoryRecord],
    vocabulary: Vocabulary,
    *,
    batch_size: int = 128,
) -> List[Prediction]:
    if not 1 <= batch_size <= 4096:
        raise ValueError("prediction batch_size must be between 1 and 4096")
    ordered = sorted(records, key=lambda record: record.trajectory_id)
    predictions: List[Prediction] = []
    model.eval()
    with torch.no_grad():
        for start in range(0, len(ordered), batch_size):
            chunk = ordered[start : start + batch_size]
            batch = collate_records(chunk, vocabulary, torch.device("cpu"))
            success_logits, value_logits = model(batch)
            success = torch.sigmoid(success_logits).tolist()
            values = torch.sigmoid(value_logits).tolist()
            predictions.extend(
                Prediction(identifier, float(probability), float(value), MODEL_SOURCE)
                for identifier, probability, value in zip(batch.trajectory_ids, success, values)
            )
    return predictions


def _log_scale(value: float, maximum: float) -> float:
    return math.log1p(value) / math.log1p(maximum)
