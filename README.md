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
- `epoch-protocol`: versioned JSONL messages at the agent/supervisor boundary.
- `epoch-supervisor`: direct execution plus restart-safe cooperative application recovery.
- `epoch-test-agent`: seeded workload for repeatable execution, tracing, and fault experiments.
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

The [CRIU compatibility guide](docs/criu-compatibility.md) documents the standalone runner,
structured matrix, Oracle ARM64 evidence, preliminary decision gates, and integration limitations.

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
