# Contributing to Epoch

Epoch is a correctness-sensitive systems experiment. Changes follow test-driven development
rather than relying on a final testing pass.

## Required development loop

For every behavior change:

1. **Red:** Add the smallest test that describes the desired behavior or reproduces the bug.
   Run it and confirm it fails for the expected reason.
2. **Green:** Implement only enough behavior to make that test pass.
3. **Refactor:** Improve structure without changing behavior, while keeping the full suite green.
4. Run formatting, strict Clippy, unit tests, and the relevant integration suite.

A bug fix without a regression test is incomplete. A security invariant must have both a positive
case and an adversarial/denied case.

## Test layers

### Unit tests

Use unit tests for:

- Lifecycle transitions.
- Canonicalization and hashing.
- Capability validation and attenuation.
- Effect-state transitions.
- Semantic normalization and diffing.
- Checkpoint metadata validation.

Unit tests must not depend on timing, network access, real credentials, or host-specific PIDs.

### Fixture tests

Linux parsers such as `/proc` and `strace` normalizers must have captured fixtures. This keeps
their parsing and normalization logic testable on macOS while the real collectors remain
Linux-only.

### Integration tests

Use integration tests for:

- SQLite durability and migrations.
- Supervisor/workload lifecycle.
- Application and workspace checkpoint/restore.
- Capability/effect gateway behavior.
- Branch and replay coordination.

### Privileged Linux tests

CRIU, namespaces, cgroup v2, seccomp, and network-boundary tests run on the dedicated Linux test
host. They must report `unsupported` separately from `passed`; an unavailable mechanism is never
a successful test.

### Fault and property tests

Crash injection and state-machine invariants must cover at least:

- Restore never increases authority.
- One operation ID commits at most one idempotent mock effect.
- Effect and event history never moves backward.
- Invalid composite checkpoints never become committed epochs.
- Replay never silently becomes fork.
- Fork never silently inherits nondelegable authority.

## Definition of done

A task is complete only when:

- The acceptance behavior is covered by tests.
- The new test failed before the implementation was added.
- `cargo fmt --check` passes.
- `cargo clippy --workspace --all-targets -- -D warnings` passes.
- `cargo test --workspace --locked` passes.
- Relevant privileged Linux tests pass or have an explicit supported/unsupported result.
- Documentation and architecture decisions reflect changed behavior.
- No credentials, tokens, private keys, or production endpoints appear in code, fixtures, logs,
  snapshots, or history.

Run the ordinary local gate with:

```bash
./scripts/check.sh
```

