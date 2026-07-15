import json

from epoch_branch_value.baselines import heuristic_baseline, random_baseline
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


def test_random_and_heuristic_baselines_are_fixed_seed_and_label_blind():
    records = generate_records(task_groups=8, branches_per_group=2, seed=11)
    assert random_baseline(records, seed=77) == random_baseline(list(reversed(records)), seed=77)
    assert heuristic_baseline(records) == heuristic_baseline(list(reversed(records)))

    flipped = [record.with_label(success=not record.label.success, value=1.0 - record.label.value) for record in records]
    assert heuristic_baseline(records) == heuristic_baseline(flipped)


def test_predictions_are_bounded_advisory_scores_without_authority_fields():
    records = generate_records(task_groups=4, branches_per_group=2, seed=7)
    for prediction in random_baseline(records, seed=1) + heuristic_baseline(records):
        encoded = prediction.as_dict()
        assert set(encoded).isdisjoint(FORBIDDEN_AUTHORITY_KEYS)
        assert set(encoded) == {
            "trajectory_id",
            "success_probability",
            "value_score",
            "source",
        }
        assert 0.0 <= prediction.success_probability <= 1.0
        assert 0.0 <= prediction.value_score <= 1.0
        assert FORBIDDEN_AUTHORITY_KEYS.isdisjoint(json.dumps(encoded).lower().split('"'))


def test_evaluation_metrics_are_explicit_and_finite():
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
