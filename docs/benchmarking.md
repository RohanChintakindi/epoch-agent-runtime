# Week 4 benchmark evidence

Epoch's benchmark path answers narrow architecture questions with reproducible raw evidence. It
does not benchmark model quality, claim that an unregistered process checkpoint backend works, or
infer external exactly-once delivery from local rollback.

## Suites

`epoch bench run <suite>` accepts five suite names:

| Suite | Evidence collected |
|---|---|
| `checkpoint` | Real Week 2 application-context plus full-copy workspace capture, validate, and restore calls; separate trace-off and trace-on reports; raw capture/restore latency and logical artifact sizes |
| `cow` | Linux fork COW allocation/fan-out/dirty-ratio probe; child minor/major faults; aggregate RSS/PSS; a real sequential full-copy control; raw samples and percentile/ratio summary |
| `compatibility` | Combined scaling rows plus typed future-schema, missing-reference, missing-workspace, special-file, and unregistered process-checkpoint rows |
| `faults` | Actual workspace restore and application capture injection hooks, followed by symbolic composite/effect stages wherever no injection or reconciliation API exists |
| `all` | Every required suite above in one evidence bundle |

Unsupported and failed matrix rows are retained in JSON and CSV. A detected executable or kernel
feature never changes a backend to supported by itself.

## Predeclared thresholds

These thresholds are compiled into `DecisionThresholds::week4()` and persisted before results are
interpreted:

| Decision input | Threshold |
|---|---:|
| Combined application + workspace capture p95 | at most 2,000,000,000 ns |
| Combined application + workspace validate/restore p95 | at most 2,000,000,000 ns |
| Correctness validation failures | 0 |
| Linux COW aggregate PSS / full-copy bytes | at most 7,500 basis points |

The checkpoint mechanism is marked `keep` only when retained samples have no failed/unsupported
outcomes, correctness validations pass, and both p95 thresholds pass. COW remains `narrow` even
when its memory threshold passes because the evidence covers one Linux/Python/fork workload, not
arbitrary process state. Transparent external exactly-once through rollback is marked `kill`
until a downstream idempotency/reconciliation API and crash evidence exist.

## Reproduce locally

Build and run a small cross-platform check:

```bash
cargo build --locked -p epoch-cli
ROOT="$(pwd)/.epoch/benchmarks"
./target/debug/epoch bench run checkpoint \
  --root "$ROOT" \
  --warmups 1 \
  --repetitions 5 \
  --fixture-bytes 1048576 \
  --fixture-files 16
```

The command prints a JSON summary containing the run ID and artifact directory. Each successful
run atomically publishes:

- `report.json`: authoritative configuration, validated environment, raw samples, matrices,
  summaries, thresholds, and decisions.
- `samples.csv`: flat rows retaining successful, unsupported, failed, and symbolic outcomes.
- `RESULTS.md`: concise keep/narrow/kill report derived from the same evidence.

Reload an artifact without rerunning its benchmark:

```bash
./target/debug/epoch bench report bench-00000000-0000-0000-0000-000000000000 \
  --root "$ROOT" \
  --format markdown
```

Replace the example ID with the emitted run ID. Formats are `json`, `csv`, and `markdown`.
Traversal IDs, symlink roots, oversized artifacts, mismatched report IDs, and unsafe allocation
requests are rejected.

## Frozen Oracle ARM64 command

The native Linux evidence command is intentionally fixed before collection:

```bash
cargo build --locked -p epoch-cli
ROOT="$(pwd)/.epoch/benchmarks"
./target/debug/epoch bench run all \
  --root "$ROOT" \
  --warmups 2 \
  --repetitions 10 \
  --fixture-bytes 1048576 \
  --fixture-files 16 \
  --seed 24301 \
  --cow-allocation-bytes 67108864 \
  --cow-children 4 \
  --cow-dirty-basis-points 2500 \
  --cow-repetitions 5
```

This requests a 64 MiB parent allocation, four children, and 25% dirty pages. The helper and Rust
front end independently reject more than 256 MiB per allocation, 16 children, 512 MiB allocation
times fan-out, ratios above 100%, or more than 100 COW repetitions.

The checked-in [Oracle ARM64 evidence index](../results/oracle-arm64/README.md) links the raw JSON,
CSV, generated decision report, environment, checksums, and concise measured summary from this
exact command.

## COW helper boundary

The auditable helper is
[`crates/epoch-bench/helpers/cow_probe.py`](../crates/epoch-bench/helpers/cow_probe.py). On Linux it:

1. allocates and touches anonymous memory;
2. times real `bytearray` copies as the full-copy control;
3. forks every child before collecting results;
4. dirties the configured page fraction per child;
5. reads RSS/PSS from `/proc/self/smaps_rollup` and faults from `getrusage`; and
6. holds children alive until all proportional-set measurements are captured.

Off Linux, without `python3`, or without `smaps_rollup`, the suite returns structured
`unsupported`. A malformed helper result or nonzero helper failure is retained as `failed`.

## Interpretation boundaries

- Trace-off and trace-on samples have distinct authoritative configurations and are never pooled.
- Reported checkpoint bytes are logical application component, workspace manifest, and restored
  file bytes; they are not a claim about compressed or incremental physical storage.
- COW RSS double-counts shared pages by definition; PSS is the comparison metric.
- The full-copy control measures bytes actually copied sequentially. It does not allocate every
  control copy simultaneously.
- Process compatibility rows remain unsupported because this revision has no registered process
  checkpoint backend.
- Effect-stage rows are symbolic where no gateway/reconciliation API exists and always set the
  external exactly-once claim to false.
