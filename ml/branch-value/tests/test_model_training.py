import json
import stat
from dataclasses import replace

import pytest
import torch

from epoch_branch_value.model import BranchValueModel, Vocabulary, collate_records
from epoch_branch_value.split import SplitConfig
from epoch_branch_value.synthetic import generate_records
from epoch_branch_value.training import (
    MAX_MODEL_JSON_BYTES,
    TrainConfig,
    evaluate_model,
    score_model,
    train_model,
)


def test_gru_features_are_only_event_categories_delta_and_two_booleans():
    records = generate_records(task_groups=3, branches_per_group=2, seed=12)
    original = records[0]
    changed_metadata = replace(
        original.with_labels(success=not original.success_label, value=1.0 - original.value_label),
        task_group_id="f" * 64,
        session_group_id="e" * 64,
        candidate_group_id="d" * 64,
        branch_depth=99,
    )
    vocabulary = Vocabulary.build(records)
    batch = collate_records([original, changed_metadata], vocabulary, device=torch.device("cpu"))
    model = BranchValueModel(vocabulary, hidden_size=16)

    assert torch.equal(batch.actor_ids[0], batch.actor_ids[1])
    assert torch.equal(batch.status_ids[0], batch.status_ids[1])
    assert torch.equal(batch.kind_ids[0], batch.kind_ids[1])
    assert torch.equal(batch.numeric[0], batch.numeric[1])
    assert batch.numeric.shape[-1] == 3
    assert not torch.equal(batch.success_targets[0], batch.success_targets[1])
    success_logits, value_logits = model(batch)
    assert success_logits.shape == (2,)
    assert value_logits.shape == (2,)
    assert model.parameter_count() < 100_000
    assert next(model.parameters()).device.type == "cpu"


def test_zero_event_and_unlabelled_trajectories_can_be_scored():
    records = generate_records(task_groups=3, branches_per_group=2, seed=6)
    empty = replace(
        records[0].with_labels(success=None, value=None),
        events=(),
        summary=records[0].summary.from_events(()),
    )
    vocabulary = Vocabulary.build(records)
    batch = collate_records([empty], vocabulary, device=torch.device("cpu"))
    model = BranchValueModel(vocabulary, hidden_size=8)
    success_logits, value_logits = model(batch)
    assert success_logits.shape == (1,)
    assert value_logits.shape == (1,)


def test_training_and_evaluation_use_only_labelled_records_and_are_reproducible(tmp_path):
    records = generate_records(task_groups=15, branches_per_group=3, seed=123)
    mixed = [
        record.with_labels(success=None, value=None) if index % 3 == 0 else record
        for index, record in enumerate(records)
    ]
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

    first = train_model(mixed, first_dir, config)
    second = train_model(list(reversed(mixed)), second_dir, config)

    assert first.as_dict() == second.as_dict()
    assert first.train_records + first.validation_records + first.test_records == 30
    first_eval = evaluate_model(mixed, first_dir, split="test")
    second_eval = evaluate_model(mixed, second_dir, split="test")
    assert first_eval.as_dict() == second_eval.as_dict()
    assert first_eval.model.source == "sequence_encoder_v1"
    assert first_eval.random.source == "random_v1"
    assert first_eval.heuristic.source == "heuristic_v1"
    assert first_eval.constant.source == "constant_train_v1"

    checkpoint_metadata = json.loads((first_dir / "model.json").read_text(encoding="utf-8"))
    assert checkpoint_metadata["format_version"] == 1
    assert checkpoint_metadata["device"] == "cpu"
    assert checkpoint_metadata["encoder"] == "gru"
    split_manifest = json.loads((first_dir / "split.json").read_text(encoding="utf-8"))
    train_ids = {
        identifier
        for identifier, split_name in split_manifest["split"]["assignment"].items()
        if split_name == "train"
    }
    train_records = [record for record in mixed if record.trajectory_id in train_ids]
    constant = checkpoint_metadata["constant_baseline"]
    assert constant["success_probability"] == sum(
        float(record.success_label) for record in train_records
    ) / len(train_records)
    assert constant["value_score"] == pytest.approx(
        sum(record.value_label for record in train_records) / len(train_records)
    )

    manifest = json.loads((first_dir / "manifest.json").read_text(encoding="utf-8"))
    assert set(manifest["files"]) >= {"model.pt", "model.json", "split.json"}
    assert stat.S_IMODE(first_dir.stat().st_mode) == 0o700
    assert all(
        stat.S_IMODE((first_dir / name).stat().st_mode) == 0o600
        for name in [*manifest["files"], "manifest.json"]
    )

    scores = score_model(mixed, first_dir)
    assert len(scores) == len(mixed)
    assert {score.trajectory_id for score in scores} == {record.trajectory_id for record in mixed}


def test_evaluation_refuses_labelled_dataset_drift_but_ignores_unlabelled_additions(tmp_path):
    records = generate_records(task_groups=8, branches_per_group=2, seed=3)
    model_dir = tmp_path / "model"
    train_model(records, model_dir, TrainConfig(seed=4, epochs=1, batch_size=4, hidden_size=8))

    with pytest.raises(ValueError, match="split manifest"):
        evaluate_model(records[:-1], model_dir, split="test")

    unlabelled = generate_records(task_groups=1, branches_per_group=1, seed=99)[0].with_labels(
        success=None, value=None
    )
    evaluate_model([*records, unlabelled], model_dir, split="test")

    with pytest.raises(ValueError, match="split"):
        evaluate_model(records, model_dir, split="everything")


def test_evaluation_recomputes_and_exactly_validates_the_persisted_split(tmp_path):
    records = generate_records(task_groups=10, branches_per_group=2, seed=42)
    model_dir = tmp_path / "model"
    train_model(records, model_dir, TrainConfig(seed=4, epochs=1, batch_size=4, hidden_size=8))

    split_path = model_dir / "split.json"
    manifest_path = model_dir / "manifest.json"
    split_document = json.loads(split_path.read_text(encoding="utf-8"))
    group = next(iter(split_document["split"]["group_assignment"]))
    original = split_document["split"]["group_assignment"][group]
    split_document["split"]["group_assignment"][group] = "test" if original != "test" else "train"
    split_path.write_text(json.dumps(split_document), encoding="utf-8")
    refresh_manifest_hash(manifest_path, split_path)
    with pytest.raises(ValueError, match="split manifest"):
        evaluate_model(records, model_dir, split="test")


def test_model_metadata_is_strict_and_its_split_config_must_match_split_manifest(tmp_path):
    records = generate_records(task_groups=8, branches_per_group=2, seed=51)
    model_dir = tmp_path / "model"
    train_model(records, model_dir, TrainConfig(seed=4, epochs=1, batch_size=4, hidden_size=8))
    model_path = model_dir / "model.json"
    manifest_path = model_dir / "manifest.json"
    original = json.loads(model_path.read_text(encoding="utf-8"))

    unexpected = {**original, "unexpected": True}
    model_path.write_text(json.dumps(unexpected), encoding="utf-8")
    refresh_manifest_hash(manifest_path, model_path)
    with pytest.raises(ValueError, match="artifact"):
        score_model(records, model_dir)

    mismatched = json.loads(json.dumps(original))
    mismatched["training"]["split"]["seed"] += 1
    model_path.write_text(json.dumps(mismatched), encoding="utf-8")
    refresh_manifest_hash(manifest_path, model_path)
    with pytest.raises(ValueError, match="split"):
        score_model(records, model_dir)


@pytest.mark.parametrize("artifact", ["model.pt", "model.json", "split.json", "manifest.json"])
def test_model_loading_normalizes_empty_truncated_oversized_and_nonregular_artifacts(
    tmp_path, artifact
):
    records = generate_records(task_groups=8, branches_per_group=2, seed=13)
    model_dir = tmp_path / "model"
    train_model(records, model_dir, TrainConfig(seed=4, epochs=1, batch_size=4, hidden_size=8))
    path = model_dir / artifact
    path.write_bytes(b"")
    with pytest.raises(ValueError, match="artifact"):
        score_model(records, model_dir)

    if artifact == "manifest.json":
        path.unlink()
        path.mkdir()
        with pytest.raises(ValueError, match="artifact"):
            score_model(records, model_dir)


def test_model_loading_rejects_oversized_artifact_before_parsing(tmp_path):
    records = generate_records(task_groups=8, branches_per_group=2, seed=14)
    model_dir = tmp_path / "model"
    train_model(records, model_dir, TrainConfig(seed=4, epochs=1, batch_size=4, hidden_size=8))
    model_json = model_dir / "model.json"
    model_json.write_bytes(b"{}" + b" " * MAX_MODEL_JSON_BYTES)
    with pytest.raises(ValueError, match="artifact"):
        score_model(records, model_dir)


def test_training_fails_clearly_without_three_labelled_task_groups(tmp_path):
    records = generate_records(task_groups=3, branches_per_group=2, seed=18)
    labelled_groups = {records[0].task_group_id, records[2].task_group_id}
    mixed = [
        record
        if record.task_group_id in labelled_groups
        else record.with_labels(success=None, value=None)
        for record in records
    ]
    with pytest.raises(ValueError, match="at least three labelled task groups"):
        train_model(mixed, tmp_path / "model", TrainConfig(epochs=1))


def test_training_refuses_a_dangling_symlink_output(tmp_path):
    records = generate_records(task_groups=3, branches_per_group=1, seed=72)
    output = tmp_path / "model"
    output.symlink_to(tmp_path / "missing-target", target_is_directory=True)

    with pytest.raises(ValueError, match="already exists"):
        train_model(records, output, TrainConfig(epochs=1, hidden_size=8))
    assert output.is_symlink()


def refresh_manifest_hash(manifest_path, changed_path):
    import hashlib

    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    payload = changed_path.read_bytes()
    manifest["files"][changed_path.name] = {
        "sha256": hashlib.sha256(payload).hexdigest(),
        "size": len(payload),
    }
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
