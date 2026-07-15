# Epoch branch value

Minimal Python 3.9+ CPU experiments for privacy-safe trajectory branch-value learning. The package
provides a strict, text-free JSONL contract, deterministic task-group splits, a pluggable sequence
encoder with a small GRU as the first implementation, fixed-seed training, and random/heuristic
baselines.

Predictions are advisory scores only. They cannot grant runtime authority or dispatch effects.
See the [experiment guide](../../docs/ml-branch-value.md) for schema assumptions, metrics, safety
boundaries, limitations, and exact `uv` commands.
