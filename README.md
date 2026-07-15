# Epoch

Epoch is a capability-aware, recoverable execution-runtime experiment for high-permission AI
agents. It is being implemented from the [runtime specification](docs/runtime-spec.md) and
[dependency-ordered implementation plan](docs/implementation-plan.md).

The first milestone is deliberately small: create typed session and branch lifecycles, persist a
durable execution history, launch a deterministic workload, and collect observable process state.
Linux isolation and checkpoint backends are added only after that vertical slice works.

## Workspace

- `epoch-blob`: atomic, SHA-256-addressed artifact storage with verified reads.
- `epoch-core`: stable identifiers, lifecycle state machines, and shared domain types.
- `epoch-cli`: command-line entry point and host capability diagnostics.

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
