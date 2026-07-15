import json
import stat

import pytest

from epoch_branch_value.baselines import (
    constant_baseline,
    heuristic_baseline,
    random_baseline,
    write_predictions_jsonl,
)
from epoch_branch_value.metrics import evaluate_predictions
from epoch_branch_value.synthetic import generate_records

FORBIDDEN_AUTHORITY_KEYS = {
    "allow",
    "capability",
    "capability_id",
    "dispatch",
    "effect",
    "grant",
    "handle",
}


def test_random_and_heuristic_baselines_are_fixed_seed_label_blind_and_accept_unlabelled():
    records = generate_records(task_groups=8, branches_per_group=2, seed=11)
    unlabelled = [record.with_labels(success=None, value=None) for record in records]

    assert random_baseline(records, seed=77) == random_baseline(list(reversed(records)), seed=77)
    assert heuristic_baseline(records) == heuristic_baseline(list(reversed(records)))
    assert heuristic_baseline(records) == heuristic_baseline(unlabelled)
    assert constant_baseline(unlabelled, 0.25, 0.75) == constant_baseline(records, 0.25, 0.75)


def test_predictions_are_strict_bounded_score_only_records_and_private_no_clobber(tmp_path):
    records = generate_records(task_groups=4, branches_per_group=2, seed=7)
    predictions = random_baseline(records, seed=1)
    output = tmp_path / "scores.jsonl"

    write_predictions_jsonl(output, predictions)

    assert stat.S_IMODE(output.stat().st_mode) == 0o600
    decoded = [json.loads(line) for line in output.read_text(encoding="utf-8").splitlines()]
    assert len(decoded) == len(records)
    for prediction, encoded in zip(predictions, decoded):
        assert set(encoded).isdisjoint(FORBIDDEN_AUTHORITY_KEYS)
        assert set(encoded) == {
            "trajectory_id",
            "success_probability",
            "value_score",
            "source",
        }
        assert 0.0 <= prediction.success_probability <= 1.0
        assert 0.0 <= prediction.value_score <= 1.0
    with pytest.raises(ValueError, match="already exists"):
        write_predictions_jsonl(output, predictions)


def test_evaluation_metrics_require_labelled_records_and_are_finite():
    records = generate_records(task_groups=6, branches_per_group=2, seed=8)
    metrics = evaluate_predictions(records, heuristic_baseline(records))
    assert set(metrics.as_dict()) == {
        "count",
        "success_accuracy",
        "success_brier",
        "success_log_loss",
        "value_mae",
        "value_rmse",
    }
    assert metrics.count == len(records)
    assert 0.0 <= metrics.success_accuracy <= 1.0
    assert metrics.success_brier >= 0.0
    assert metrics.success_log_loss >= 0.0
    assert metrics.value_mae >= 0.0
    assert metrics.value_rmse >= 0.0

    unlabelled = records[0].with_labels(success=None, value=None)
    with pytest.raises(ValueError, match="labelled"):
        evaluate_predictions([unlabelled], heuristic_baseline([unlabelled]))


def test_synthetic_labels_have_only_noisy_intermediate_correlations():
    records = generate_records(task_groups=80, branches_per_group=2, seed=91)
    terminal_shapes = {
        (
            record.events[-1].actor,
            record.events[-1].kind,
            record.events[-1].status,
            record.events[-1].references_epoch,
            record.events[-1].has_causal_parent,
        )
        for record in records
    }
    assert terminal_shapes == {("supervisor", "safe_point", "unknown", True, True)}

    failed_signal_by_label = {True: set(), False: set()}
    for record in records:
        failed_signal_by_label[record.success_label].add(
            any(event.status == "failed" for event in record.events[:-1])
        )
    assert failed_signal_by_label == {True: {True, False}, False: {True, False}}
