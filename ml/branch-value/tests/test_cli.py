import json
import stat
import subprocess
import sys

from epoch_branch_value.schema import load_jsonl, write_jsonl


def run_cli(*arguments):
    return subprocess.run(
        [sys.executable, "-m", "epoch_branch_value.cli", *map(str, arguments)],
        check=False,
        capture_output=True,
        text=True,
    )


def test_generate_validate_train_evaluate_and_score_cli(tmp_path):
    dataset = tmp_path / "synthetic.jsonl"
    generated = run_cli(
        "generate",
        "--output",
        dataset,
        "--task-groups",
        12,
        "--branches-per-group",
        3,
        "--seed",
        44,
    )
    assert generated.returncode == 0, generated.stderr
    assert stat.S_IMODE(dataset.stat().st_mode) == 0o600
    records = load_jsonl(dataset)
    assert len(records) == 36

    mixed = [
        record.with_labels(success=None, value=None) if index % 3 == 0 else record
        for index, record in enumerate(records)
    ]
    mixed_dataset = tmp_path / "mixed.jsonl"
    write_jsonl(mixed_dataset, mixed)
    validated = run_cli("validate", mixed_dataset)
    assert validated.returncode == 0, validated.stderr
    validation = json.loads(validated.stdout)
    assert validation["records"] == 36
    assert validation["labelled_records"] == 24
    assert validation["events"] > 0

    model_dir = tmp_path / "model"
    trained = run_cli(
        "train",
        mixed_dataset,
        "--output-dir",
        model_dir,
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

    evaluated = run_cli("evaluate", mixed_dataset, "--model-dir", model_dir, "--split", "test")
    assert evaluated.returncode == 0, evaluated.stderr
    assert set(json.loads(evaluated.stdout)) == {
        "schema_version",
        "split",
        "model",
        "random",
        "heuristic",
        "constant",
    }

    scores = tmp_path / "scores.jsonl"
    scored = run_cli("score", mixed_dataset, "--model-dir", model_dir, "--output", scores)
    assert scored.returncode == 0, scored.stderr
    assert stat.S_IMODE(scores.stat().st_mode) == 0o600
    decoded = [json.loads(line) for line in scores.read_text(encoding="utf-8").splitlines()]
    assert len(decoded) == len(mixed)
    assert all(
        set(item) == {"trajectory_id", "success_probability", "value_score", "source"}
        for item in decoded
    )
    refused = run_cli("score", mixed_dataset, "--model-dir", model_dir, "--output", scores)
    assert refused.returncode == 2
    assert "already exists" in refused.stderr


def test_cli_returns_clean_validation_errors(tmp_path):
    invalid = tmp_path / "invalid.jsonl"
    invalid.write_text('{"schema_version":99}\n', encoding="utf-8")
    result = run_cli("validate", invalid)
    assert result.returncode == 2
    assert "validation failed" in result.stderr
    assert result.stdout == ""


def test_cli_normalizes_an_overflowing_integer_label_to_exit_two(tmp_path):
    dataset = tmp_path / "overflowing-label.jsonl"
    assert run_cli("generate", "--output", dataset, "--task-groups", 1).returncode == 0
    value = json.loads(dataset.read_text(encoding="utf-8").splitlines()[0])
    value["value_label"] = int("9" * 400)
    dataset.write_text(json.dumps(value) + "\n", encoding="utf-8")

    result = run_cli("validate", dataset)
    assert result.returncode == 2
    assert "validation failed" in result.stderr
    assert "value_label" in result.stderr
    assert "Traceback" not in result.stderr
    assert result.stdout == ""


def test_cli_normalizes_broken_model_artifacts_to_exit_two(tmp_path):
    dataset = tmp_path / "synthetic.jsonl"
    assert run_cli("generate", "--output", dataset, "--task-groups", 6).returncode == 0
    model = tmp_path / "model"
    assert run_cli("train", dataset, "--output-dir", model, "--epochs", 1).returncode == 0
    (model / "model.pt").write_bytes(b"truncated")

    scores = tmp_path / "scores.jsonl"
    result = run_cli("score", dataset, "--model-dir", model, "--output", scores)
    assert result.returncode == 2
    assert "artifact" in result.stderr
    assert result.stdout == ""
    assert not scores.exists()
