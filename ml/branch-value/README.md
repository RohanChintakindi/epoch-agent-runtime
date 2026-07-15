# Epoch branch value

Minimal Python 3.9+ CPU experiments for privacy-safe trajectory branch-value learning. The package
consumes the exact Rust metadata-only JSONL contract, performs deterministic labelled task-group
splits, and provides a pluggable sequence encoder with a small GRU as the first implementation.
Training is fixed-seed and compares random, heuristic, and training-only constant baselines.

Predictions are advisory scores only. They cannot grant runtime authority or dispatch effects.
See the [experiment guide](../../docs/ml-branch-value.md) for schema assumptions, metrics, safety
boundaries, verified model bundles, limitations, and exact `uv` commands.
