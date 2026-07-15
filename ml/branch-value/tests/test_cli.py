import json
import subprocess
import sys

from epoch_branch_value.schema import load_jsonl


def run_cli(*arguments):
    return subprocess.run(
        [sys.executable, "-m", "epoch_branch_value.cli", *map(str, arguments)],
        check=False,
        capture_output=True,
        text=True,
    )


def test_generate_validate_train_evaluate_cli_is_reproducible(tmp_path):
    dataset = tmp_path / "synthetic.jsonl"
    generated = run_cli(
        "generate",
        "--output",
        dataset,
        "--task-groups",
        12,
        "--branches-per-group",
        2,
        "--seed",
        44,
    )
    assert generated.returncode == 0, generated.stderr
    assert len(load_jsonl(dataset)) == 24

    validated = run_cli("validate", dataset)
    assert validated.returncode == 0, validated.stderr
    assert json.loads(validated.stdout)["records"] == 24

    first = tmp_path / "model-first"
    second = tmp_path / "model-second"
    for output in (first, second):
        trained = run_cli(
            "train",
            dataset,
            "--output-dir",
            output,
            "--seed",
            7,
            "--split-seed",
            8,
            "--epochs",
            1,
            "--batch-size",
            8,
            "--hidden-size",
            8,
        )
        assert trained.returncode == 0, trained.stderr

    first_metrics = json.loads((first / "training-metrics.json").read_text(encoding="utf-8"))
    second_metrics = json.loads((second / "training-metrics.json").read_text(encoding="utf-8"))
    assert first_metrics == second_metrics

    evaluated = run_cli("evaluate", dataset, "--model-dir", first, "--split", "test")
    assert evaluated.returncode == 0, evaluated.stderr
    report = json.loads(evaluated.stdout)
    assert set(report) == {"schema_version", "split", "model", "random", "heuristic"}
    assert "capability" not in evaluated.stdout.lower()
    assert "effect" not in evaluated.stdout.lower()


def test_cli_returns_clean_validation_errors(tmp_path):
    invalid = tmp_path / "invalid.jsonl"
    invalid.write_text('{"schema_version":99}\n', encoding="utf-8")
    result = run_cli("validate", invalid)
    assert result.returncode == 2
    assert "validation failed" in result.stderr
    assert result.stdout == ""
