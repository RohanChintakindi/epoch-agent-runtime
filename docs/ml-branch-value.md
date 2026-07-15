# Privacy-safe trajectory branch-value experiment

`ml/branch-value` is an offline research harness for one bounded question: can Epoch's normalized
execution metadata predict branch success and value? It consumes the exact
[Rust trajectory schema](trajectory-schema.md). It is not in the runtime authority path. Its only
output is an opaque trajectory ID plus two bounded scores and a source name; it cannot grant a
capability, authorize or dispatch an effect, select a provider, mutate a checkpoint, or promote a
branch.

The first encoder is a small, single-layer CPU GRU behind a pluggable sequence-encoder interface.
That is the appropriate starting point for a small initial dataset and at most 256 events. A
Transformer is deliberately not implemented in the first experiment.

## Exact Rust record contract

Input is UTF-8 JSONL with one schema-version-1 trajectory per newline. The reader uses
`readline(256 KiB + 1)` so a malicious unterminated line cannot force an unbounded allocation. It
rejects oversized, blank, unterminated, non-UTF-8, duplicate-key, duplicate-trajectory, unknown-
field, and invalid records. The dataset writer creates a new canonical file with mode `0600` and
never replaces an existing path.

Every record has exactly:

- `schema_version`: integer `1`.
- `privacy_profile`: string `metadata_only`.
- `trajectory_id`, `task_group_id`, `session_group_id`, and `candidate_group_id`: exactly 64
  lowercase hexadecimal characters. Rust produces domain-separated SHA-256 pseudonyms.
- `branch_depth`: integer in the Rust `u32` range.
- `success_label`: Boolean for a labelled terminal branch, otherwise `null`.
- `value_label`: finite number in `[0, 1]` for a labelled terminal branch, otherwise `null`.
- `events`: zero to 256 metadata-only event objects.
- `summary`: the exact counters and duration derived from those events.

The two labels must either both be present or both be `null`. The Python reader never accepts a
partial label. Version 1 inherits Rust's explicit experimental outcome proxy: promoted is
`(true, 1.0)`, completed is `(true, 0.75)`, and failed or abandoned is `(false, 0.0)`. This validates
the learning machinery; it is not a production utility definition.

Every event has exactly:

- `position`: contiguous integer starting at zero.
- `delta_monotonic_ns`: integer in the Rust `u64` range; the first event's delta is zero.
- `actor`: `agent`, `supervisor`, `tool`, `gateway`, or `operator`.
- `kind`: exactly one member of Rust's finite taxonomy: `agent.start`, `context.update`,
  `model.request`, `model.response`, `tool.call`, `tool.result`, `safe_point`,
  `supervisor.run_started`, `process.started`, `process.manifest`, `process.stderr`,
  `application.context_restored`, or `other`.
- `status`: `started`, `succeeded`, `failed`, `denied`, or `unknown`.
- `references_epoch` and `has_causal_parent`: Booleans.

The finite kind taxonomy prevents an arbitrary event name from becoming a covert text channel or
an unbounded learned vocabulary. Rust truncates the feature timeline before the first terminal
outcome event (`agent.completion`, `process.exited`, or `supervisor.failure`), dropping that event
and everything after it. Python also rejects those kinds because they are not in the taxonomy.

The summary has exactly `event_count`, `duration_monotonic_ns`, `started_events`,
`succeeded_events`, `failed_events`, `denied_events`, and `unknown_events`. Python recomputes all
seven values and rejects any mismatch.

This allowlist cannot represent prompts, reasoning text, payloads, tool arguments or results,
paths, URLs, email addresses, blob hashes, capability handles, effect arguments, credentials, raw
runtime IDs, branch state, or arbitrary metadata. A pseudonym is still sensitive linkable data;
experiments should default to synthetic or explicitly consented, access-controlled records. In
particular, a predictable low-entropy task token may still be guessed from its digest.

Rust and Python both load `crates/epoch-trajectory/tests/fixtures/schema-v1.jsonl`, containing one
labelled nonempty trajectory and one unlabelled empty trajectory. This makes wire drift fail in
both language test suites.

## Labels, grouping, and leakage controls

Training and metrics use labelled records only. Scoring accepts labelled or unlabelled records.
The splitter fails clearly unless labelled data spans at least three task groups and produces
nonempty train, validation, and test partitions.

`task_group_id` is the partition unit, so executions of the same underlying task stay together.
Sibling candidates share `candidate_group_id`; the reader and splitter reject a candidate group
that crosses task groups. Sorting before a fixed-seed shuffle makes assignment independent of input
order.

The bundle stores the split configuration, unit, complete group assignment, complete labelled-
trajectory assignment, and SHA-256 fingerprint of the canonical labelled dataset. Evaluation does
not trust those fields: it parses the stored `SplitConfig`, recomputes `split_by_task_group`, and
requires exact equality of configuration, unit, group mapping, and record mapping. Labelled dataset
drift fails evaluation. Adding unrelated unlabelled records does not change an offline metric.

## Model inputs and comparisons

The GRU receives only, per event:

- actor, status, and finite-kind embeddings;
- log-scaled `delta_monotonic_ns`;
- `references_epoch` and `has_causal_parent` as two Boolean features.

It does not receive either label, summary outcome counters, any opaque identifier, branch depth, or
terminal branch state. Empty trajectories use one all-padding timestep so they can still be scored.
Two linear heads produce sigmoid-bounded success probability and value score. Training minimizes
binary cross entropy for success plus mean squared error for value.

Every evaluation compares the same labelled records against:

- the GRU;
- a fixed-seed, per-ID random baseline;
- a label-blind status heuristic;
- a constant baseline whose success prevalence and mean value come only from the training split.

Reported metrics are success accuracy at `0.5`, success Brier score, binary log loss, value mean
absolute error, and value root mean squared error. Lower is better for every metric except accuracy.
Synthetic fixtures contain noisy intermediate correlations and an identical non-outcome terminal
event shape for both labels. They test mechanics and leakage controls, not real predictive value.

## Model bundle integrity

Training refuses to overwrite a model directory. It builds a private `0700` staging directory,
writes `0600` regular files, and publishes the completed directory as one rename. The bundle has:

- `model.pt`: CPU PyTorch state dictionary loaded with `weights_only=True`;
- `model.json`: fixed GRU architecture, finite vocabulary, training configuration, and training-only
  constant baseline;
- `split.json`: labelled dataset digest and exact split;
- `training-metrics.json`: training result and validation comparison;
- `manifest.json`: byte size and SHA-256 for every file above.

Loading rejects missing, empty, non-regular, symlinked, oversized, truncated, hash-mismatched, or
malformed artifacts before scoring. Each artifact has a conservative byte limit. CLI failures are
normalized to a clean exit status `2`, and `score` never creates output if model validation fails.
Manifest hashes detect corruption or substitution relative to that manifest; they are not a
signature or authenticity guarantee against an attacker who can rewrite the entire bundle.

## Reproduce locally

Python 3.9 or newer and `uv` are required. Training is fixed-seed and CPU-only. The lock maps Linux
Torch to PyTorch's explicit CPU wheel index, contains no CUDA/NVIDIA packages, and uses the normal
macOS wheel on macOS.

Choose fresh paths because dataset, score, and model writers all refuse replacement:

```bash
cd ml/branch-value
uv sync --locked --all-groups

uv run epoch-branch-value generate \
  --output /tmp/epoch-trajectories-new.jsonl \
  --task-groups 30 \
  --branches-per-group 2 \
  --seed 101

uv run epoch-branch-value validate /tmp/epoch-trajectories-new.jsonl

uv run epoch-branch-value train /tmp/epoch-trajectories-new.jsonl \
  --output-dir /tmp/epoch-branch-value-model-new \
  --seed 23 \
  --split-seed 17 \
  --epochs 10

uv run epoch-branch-value evaluate /tmp/epoch-trajectories-new.jsonl \
  --model-dir /tmp/epoch-branch-value-model-new \
  --split test

uv run epoch-branch-value score /tmp/epoch-trajectories-new.jsonl \
  --model-dir /tmp/epoch-branch-value-model-new \
  --output /tmp/epoch-branch-value-scores-new.jsonl

uv run pytest
uv run ruff check .
uv run ruff format --check .
```

Each score line contains exactly `trajectory_id`, `success_probability`, `value_score`, and
`source`. No score can carry runtime authority.

## Controlled next experiment

Only after collecting enough consented, metadata-only trajectories should a small Transformer be
registered as a second encoder. Freeze the stored task-group split; keep inputs, baselines, metrics,
parameter-count reporting, and CPU-latency measurement identical; pre-register the improvement
threshold; and discard the direction if it does not improve out-of-group results. Calibration,
distribution-shift checks, and independent safety review remain required before any downstream use.
