# Epoch

Epoch is a capability-aware, recoverable execution-runtime experiment for high-permission AI
agents. It is being implemented from the [runtime specification](docs/runtime-spec.md) and
[dependency-ordered implementation plan](docs/implementation-plan.md).

The first milestone is deliberately small: create typed session and branch lifecycles, persist a
durable execution history, launch a deterministic workload, and collect observable process state.
Linux isolation and checkpoint backends are added only after that vertical slice works.

## Workspace

- `epoch-blob`: private, atomic SHA-256 storage with bounded verified reads and media-type locking.
- `epoch-checkpoint`: versioned, blob-backed observable application checkpoints.
- `epoch-criu-compat`: bounded CRIU dump/restore compatibility and scaling evidence runner.
- `epoch-core`: stable identifiers, lifecycle state machines, and shared domain types.
- `epoch-events`: append-only execution history with deterministic queries and external payloads.
- `ml/branch-value`: privacy-safe, CPU-only GRU branch-value experiments with fixed splits and
  baseline evaluation; its scores have no runtime authority.
- `epoch-protocol`: versioned JSONL messages at the agent/supervisor boundary.
- `epoch-sandbox`: fail-closed direct/Linux backend contracts and a native-tested Linux isolation
  prototype.
- `epoch-supervisor`: direct execution plus restart-safe cooperative application recovery.
- `epoch-test-agent`: seeded workload for repeatable execution, tracing, and fault experiments.
- `epoch-trajectory`: metadata-only, outcome-cutoff trajectory export with opaque grouping IDs.
- `epoch-workspace`: deterministic full-copy workspace snapshots and no-clobber restore.
- `epoch-cli`: command-line entry point and host capability diagnostics.

The wire contract and its forward-compatibility rules are documented in the
[agent boundary protocol](docs/agent-boundary-protocol.md).

Run the deterministic workload without any credentials or external service:

```bash
cargo run -p epoch-test-agent -- \
  --scenario full \
  --seed 24301 \
  --workspace .epoch/workload
```

Its JSONL boundary history is written to stdout and its normalized state, trace hashes, and raw
cooperative checkpoint context are written as one JSON object to stderr. See the
[deterministic agent guide](docs/deterministic-agent.md) for scenarios and crash points.

The [application checkpoint guide](docs/application-checkpoints.md) and
[workspace checkpoint guide](docs/workspace-checkpoint.md) document the Week 2 composite
checkpoint/restore/status/diff flow and its explicit process-memory limitations.

The [logical fork and replay boundary](docs/fork-replay.md) documents durable branch lineage,
restart-safe inspection, recorded-result evidence, and the explicit replay, effect-frontier, and
promotion limitations.

The [Linux sandbox guide](docs/linux-sandbox.md) documents backend discovery, namespace/cgroup/
seccomp boundaries, the threat model, native Oracle validation, measured launch samples, and the
prototype's explicit limitations.

The [five-minute interview demo](docs/interview-demo.md) provides the exact reproducible command,
talk track, evidence contract, safety rules, and integration hooks for the implemented vertical
slice.

The [Week 4 benchmark guide](docs/benchmarking.md) documents the real checkpoint, compatibility,
COW-memory, and fault suites; frozen thresholds; stable artifacts; safety bounds; and the exact
Oracle ARM64 evidence command.

The [final performance matrix](docs/final-performance-matrix.md) defines the complete 60-key COW
campaign and direct-vs-Linux cold/warm isolation comparison embedded by `epoch bench run all`.

The [CRIU compatibility guide](docs/criu-compatibility.md) documents the standalone runner,
structured matrix, Oracle ARM64 evidence, preliminary decision gates, and integration limitations.

The [branch-value experiment guide](docs/ml-branch-value.md) documents the strict text-free dataset,
task-group leakage boundary, fixed-seed CPU GRU, baselines, metrics, and non-authoritative output.
Its exact cross-language contract is frozen in the [trajectory schema](docs/trajectory-schema.md),
and the [ML smoke guide](docs/ml-demo.md) exercises Rust export, Python validation, training,
evaluation, and advisory scoring as one credential-free flow.

The [final Week 4 acceptance runbook](docs/final-runbook.md) is the single demo, benchmark, Linux,
CRIU, evidence, and interview-Q&A gate for a clean candidate revision.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p epoch-cli -- doctor
```

Real namespace, cgroup, `strace`, and CRIU tests require the documented Linux execution host.
On macOS, `epoch doctor` reports control-plane-only capability rather than treating unavailable
Linux mechanisms as successful.
