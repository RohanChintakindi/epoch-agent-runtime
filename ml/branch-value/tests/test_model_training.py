import json

import pytest
import torch

from epoch_branch_value.model import BranchValueModel, Vocabulary, collate_records
from epoch_branch_value.split import SplitConfig
from epoch_branch_value.synthetic import generate_records
from epoch_branch_value.training import TrainConfig, evaluate_model, train_model


def test_small_cpu_sequence_encoder_has_two_bounded_advisory_heads():
    records = generate_records(task_groups=3, branches_per_group=2, seed=12)
    vocabulary = Vocabulary.build(records)
    batch = collate_records(records, vocabulary, device=torch.device("cpu"))
    model = BranchValueModel(vocabulary, hidden_size=16)

    success_logits, value_logits = model(batch)

    assert success_logits.shape == (len(records),)
    assert value_logits.shape == (len(records),)
    assert model.parameter_count() < 100_000
    assert next(model.parameters()).device.type == "cpu"
    assert torch.all((torch.sigmoid(success_logits) >= 0) & (torch.sigmoid(success_logits) <= 1))
    assert torch.all((torch.sigmoid(value_logits) >= 0) & (torch.sigmoid(value_logits) <= 1))


def test_fixed_seed_training_and_evaluation_are_reproducible(tmp_path):
    records = generate_records(task_groups=15, branches_per_group=2, seed=123)
    config = TrainConfig(
        seed=19,
        epochs=2,
        batch_size=8,
        learning_rate=0.01,
        hidden_size=16,
        split=SplitConfig(seed=31, train_ratio=0.6, validation_ratio=0.2),
    )
    first_dir = tmp_path / "first"
    second_dir = tmp_path / "second"

    first = train_model(records, first_dir, config)
    second = train_model(list(reversed(records)), second_dir, config)

    assert first.as_dict() == second.as_dict()
    first_eval = evaluate_model(records, first_dir, split="test")
    second_eval = evaluate_model(records, second_dir, split="test")
    assert first_eval.as_dict() == second_eval.as_dict()
    assert first_eval.model.source == "sequence_encoder_v1"
    assert first_eval.random.source == "random_v1"
    assert first_eval.heuristic.source == "heuristic_v1"

    checkpoint_metadata = json.loads((first_dir / "model.json").read_text(encoding="utf-8"))
    assert checkpoint_metadata["format_version"] == 1
    assert checkpoint_metadata["device"] == "cpu"
    assert "capability" not in json.dumps(first_eval.as_dict()).lower()
    assert "effect" not in json.dumps(first_eval.as_dict()).lower()


def test_evaluation_refuses_dataset_or_split_drift(tmp_path):
    records = generate_records(task_groups=8, branches_per_group=2, seed=3)
    model_dir = tmp_path / "model"
    train_model(records, model_dir, TrainConfig(seed=4, epochs=1, batch_size=4, hidden_size=8))

    with pytest.raises(ValueError, match="split manifest"):
        evaluate_model(records[:-1], model_dir, split="test")
    with pytest.raises(ValueError, match="split"):
        evaluate_model(records, model_dir, split="everything")
