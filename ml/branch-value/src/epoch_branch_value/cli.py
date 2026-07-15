"""Fixed-seed command line interface for bounded branch-value experiments."""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import Any, Dict, Optional

from .baselines import write_predictions_jsonl
from .schema import DatasetValidationError, load_jsonl, write_jsonl
from .split import SplitConfig
from .synthetic import generate_records
from .training import TrainConfig, evaluate_model, score_model, train_model


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="epoch-branch-value",
        description="Privacy-safe CPU branch-value experiments for Epoch trajectories",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    generate = commands.add_parser("generate", help="write deterministic synthetic JSONL")
    generate.add_argument("--output", type=Path, required=True)
    generate.add_argument("--task-groups", type=int, default=30)
    generate.add_argument("--branches-per-group", type=int, default=2)
    generate.add_argument("--seed", type=int, default=101)

    validate = commands.add_parser("validate", help="validate a versioned JSONL dataset")
    validate.add_argument("dataset", type=Path)
    validate.add_argument("--max-records", type=int, default=100_000)

    train = commands.add_parser("train", help="train a fixed-seed CPU GRU experiment")
    train.add_argument("dataset", type=Path)
    train.add_argument("--output-dir", type=Path, required=True)
    train.add_argument("--seed", type=int, default=23)
    train.add_argument("--split-seed", type=int, default=17)
    train.add_argument("--epochs", type=int, default=10)
    train.add_argument("--batch-size", type=int, default=32)
    train.add_argument("--learning-rate", type=float, default=0.003)
    train.add_argument("--hidden-size", type=int, default=32)
    train.add_argument("--train-ratio", type=float, default=0.7)
    train.add_argument("--validation-ratio", type=float, default=0.15)

    evaluate = commands.add_parser("evaluate", help="compare a trained GRU with fixed baselines")
    evaluate.add_argument("dataset", type=Path)
    evaluate.add_argument("--model-dir", type=Path, required=True)
    evaluate.add_argument("--split", choices=("train", "validation", "test"), default="test")

    score = commands.add_parser(
        "score", help="write strict score-only JSONL for labelled or unlabelled trajectories"
    )
    score.add_argument("dataset", type=Path)
    score.add_argument("--model-dir", type=Path, required=True)
    score.add_argument("--output", type=Path, required=True)
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = build_parser()
    arguments = parser.parse_args(argv)
    try:
        report = _execute(arguments)
    except (DatasetValidationError, ValueError, OSError, RuntimeError) as error:
        operation = "validation" if arguments.command == "validate" else arguments.command
        print(f"{operation} failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps(report, sort_keys=True, allow_nan=False))
    return 0


def _execute(arguments: argparse.Namespace) -> Dict[str, Any]:
    if arguments.command == "generate":
        records = generate_records(
            task_groups=arguments.task_groups,
            branches_per_group=arguments.branches_per_group,
            seed=arguments.seed,
        )
        write_jsonl(arguments.output, records)
        return {
            "schema_version": 1,
            "records": len(records),
            "task_groups": arguments.task_groups,
            "branches_per_group": arguments.branches_per_group,
            "seed": arguments.seed,
            "output": str(arguments.output),
        }
    if arguments.command == "validate":
        records = load_jsonl(arguments.dataset, max_records=arguments.max_records)
        return {
            "schema_version": 1,
            "records": len(records),
            "labelled_records": sum(record.is_labelled for record in records),
            "task_groups": len({record.task_group_id for record in records}),
            "events": sum(len(record.events) for record in records),
        }
    if arguments.command == "train":
        records = load_jsonl(arguments.dataset)
        config = TrainConfig(
            seed=arguments.seed,
            epochs=arguments.epochs,
            batch_size=arguments.batch_size,
            learning_rate=arguments.learning_rate,
            hidden_size=arguments.hidden_size,
            split=SplitConfig(
                seed=arguments.split_seed,
                train_ratio=arguments.train_ratio,
                validation_ratio=arguments.validation_ratio,
            ),
        )
        return train_model(records, arguments.output_dir, config).as_dict()
    if arguments.command == "evaluate":
        records = load_jsonl(arguments.dataset)
        return evaluate_model(records, arguments.model_dir, split=arguments.split).as_dict()
    if arguments.command == "score":
        records = load_jsonl(arguments.dataset)
        predictions = score_model(records, arguments.model_dir)
        write_predictions_jsonl(arguments.output, predictions)
        return {
            "schema_version": 1,
            "records": len(predictions),
            "output": str(arguments.output),
            "source": predictions[0].source,
        }
    raise ValueError(f"unsupported command {arguments.command!r}")


if __name__ == "__main__":
    raise SystemExit(main())
