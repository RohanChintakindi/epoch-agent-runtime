# Privacy-safe trajectory branch-value experiment

This package is an offline research harness for one narrow question: can normalized execution
telemetry predict whether an agent branch will succeed and how valuable its outcome will be? It is
not part of the runtime authority path. A model prediction is only a pair of bounded scores and
cannot grant a capability, authorize or dispatch an effect, select a provider, mutate a checkpoint,
or promote a branch.

The first encoder is a small, single-layer GRU. That is the appropriate starting point for the
expected small dataset and trajectories of at most 256 steps. The encoder is behind a pluggable
interface so a Transformer can be tested later as a controlled comparison. That comparison must
use the exact persisted task-group split, preprocessing, baselines, and metrics described here; a
Transformer is intentionally not implemented in this first experiment.

## Privacy and schema contract

Input is UTF-8 JSONL with one schema-version-1 trajectory per newline. The reader rejects blank or
unterminated records, duplicate JSON keys, duplicate trajectory IDs, unknown fields, non-UTF-8
data, records larger than 256 KiB, datasets over the configured record limit, and invalid numeric
bounds. Writers validate typed records and atomically replace the destination with canonical JSON.

Each trajectory has exactly these fields:

- `schema_version`: integer `1`.
- `trajectory_id`: canonical UUIDv4, unique in the dataset.
- `task_group_id`: opaque `tg_` plus 16–64 lowercase hexadecimal characters. All branches derived
  from the same original task must share this value.
- `branch_id`: canonical UUIDv4.
- `parent_branch_id`: canonical UUIDv4 or `null`.
- `steps`: 1–256 normalized step objects with contiguous sequence numbers starting at zero.
- `label`: a Boolean `success` and finite normalized `value` in `[0, 1]`.

A step has only categorical or bounded numeric telemetry:

- `sequence`: integer in `[0, 255]`.
- `actor`: `agent`, `supervisor`, `tool`, `gateway`, or `operator`.
- `kind`: a normalized lowercase categorical token matching `[a-z0-9][a-z0-9._]{0,63}`.
- `status`: `started`, `succeeded`, `failed`, `denied`, or `unknown`.
- `duration_ms`: finite number in `[0, 3,600,000]`.
- `token_count`: integer in `[0, 1,000,000]`.
- `effect_count` and `capability_count`: integers in `[0, 1,000]`.

The strict allowlist deliberately cannot represent prompts, reasoning text, tool arguments or
results, provider responses, paths, URLs, email addresses, authorization handles, secrets, or
arbitrary metadata. Producers must de-identify records before writing them and should default to
synthetic or explicitly consented experimental data. An opaque identifier prevents accidental
disclosure in this schema; it does not make a reversibly encoded or poorly chosen identifier safe.

Labels are experimental ground truth. `success` must mean the task-level outcome chosen before
training, and `value` must use one stable rubric across all splits. Changing either definition
creates a new dataset version rather than silently relabeling an existing evaluation.

## Leakage-resistant evaluation

Splitting happens by `task_group_id`, never by individual branch. Sibling branches therefore stay
together in train, validation, or test and cannot leak near-duplicate task information across
partitions. Sorting before a fixed-seed shuffle makes assignment independent of input order. The
training artifact stores every group and trajectory assignment plus a SHA-256 fingerprint of the
canonical dataset; evaluation fails if the dataset or assignment drifts.

The train partition alone builds categorical vocabularies. Unseen step kinds map to an explicit
unknown token. Both the learned model and baselines are evaluated against the same records using:

- success accuracy at a probability threshold of `0.5`;
- success Brier score and binary log loss (lower is better);
- value mean absolute error and root mean squared error (lower is better).

The random baseline derives stable per-trajectory scores from a fixed seed and opaque trajectory
ID. The deterministic heuristic uses only normalized step statuses. Neither baseline reads labels
while predicting. The GRU trains two heads with binary cross entropy for success and mean squared
error on the sigmoid-bounded value score.

These metrics establish a reproducible comparison, not production readiness. Synthetic data can
test mechanics but cannot establish real-world predictive value. Any later decision gate should
predefine minimum improvement over both baselines, calibration and distribution-shift checks, and
an independent safety review. Runtime policy must continue to enforce authority independently of
model output.

## Reproduce locally

Python 3.9 or newer and `uv` are required. Training is fixed-seed and CPU-only.

```bash
cd ml/branch-value
uv sync --locked --all-groups

uv run epoch-branch-value generate \
  --output /tmp/epoch-trajectories.jsonl \
  --task-groups 30 \
  --branches-per-group 2 \
  --seed 101

uv run epoch-branch-value validate /tmp/epoch-trajectories.jsonl

uv run epoch-branch-value train /tmp/epoch-trajectories.jsonl \
  --output-dir /tmp/epoch-branch-value-model \
  --seed 23 \
  --split-seed 17 \
  --epochs 10

uv run epoch-branch-value evaluate /tmp/epoch-trajectories.jsonl \
  --model-dir /tmp/epoch-branch-value-model \
  --split test

uv run pytest
uv run ruff check .
uv run ruff format --check .
```

Training refuses to overwrite an existing output directory. It produces CPU weights in `model.pt`,
architecture and vocabulary metadata in `model.json`, the immutable dataset fingerprint and exact
split in `split.json`, and validation results in `training-metrics.json`. Evaluation emits one JSON
object comparing the GRU, random baseline, and heuristic baseline.

## Controlled next experiment

Only after collecting enough consented, de-identified trajectories should a small Transformer be
registered as a second encoder. Keep the stored task-group assignment frozen, match the GRU's input
features and evaluation metrics, report parameter count and CPU latency alongside quality, and
discard the direction if it does not produce a meaningful out-of-group improvement.
