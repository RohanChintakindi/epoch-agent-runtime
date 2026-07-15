# Deterministic test agent

`epoch-test-agent` is the reproducible workload used before Epoch runs a live model. It exercises
the version 1 agent boundary protocol and produces evidence the supervisor can ingest without API
keys, credentials, internet access, or nondeterministic model output.

## What a full run exercises

A `full` run:

1. Emits agent start and initial context records.
2. Emits a model request and uses a response from the checked-in recorded-response fixture.
3. Creates `artifact.txt` and appends a seeded mutation.
4. Allocates a bounded buffer and fills it with a repository-owned SplitMix64 stream.
5. Spawns `/bin/sh` as a real child process and hashes its fixed stdout.
6. Opens a loopback TCP listener, sends a seeded request, and records the fixed response without an
   external service.
7. Emits the final context, safe point, and successful completion.

The `files`, `memory`, `child`, and `network` scenarios run only the named mechanism while retaining
the common model and lifecycle boundary events. Select one with `--scenario`.

```bash
cargo run -p epoch-test-agent -- \
  --scenario files \
  --seed 99 \
  --workspace /tmp/epoch-files
```

## Determinism contract

The seed controls stable IDs, file mutations, memory bytes, and the loopback payload. The normalized
state contains hashes and semantic values, never the workspace's absolute path, a process ID, or an
ephemeral TCP port. Hash values use the storage layer's canonical bare 64-character lowercase
SHA-256 representation. Consequently, runs with the same configuration in different directories
emit byte-identical JSONL and identical state and trace hashes.

The test agent does not claim instruction-level replay. Its boundary history is deterministic so
Epoch can test recorded-boundary replay, checkpoint coordination, and semantic state diffing around
a controlled workload.

The default allocation is 64 KiB and `--memory-bytes` is capped at 16 MiB. This keeps a malformed
or accidental command from turning the fixture into an unbounded memory workload.

## Output channels

- stdout contains only newline-terminated version 1 protocol records. Every record is flushed before
  the next action, so a supervisor can retain the valid prefix after a crash.
- stderr contains one JSON `RunSummary` on success. It includes normalized state, `state_hash`,
  `normalized_trace_hash`, and `event_count`.
- No raw model/tool content, credentials, host paths, PIDs, or listener ports are written to the
  boundary stream.

## Crash injection

Use `--crash-at after-model`, `after-first-tool`, or `after-safe-point`. The agent flushes the last
boundary record, emits no completion, reports the injected point on stderr, and exits with status
70. The resulting JSONL prefix remains valid and deterministic.

```bash
cargo run -p epoch-test-agent -- \
  --scenario full \
  --seed 99 \
  --workspace /tmp/epoch-crash \
  --crash-at after-safe-point
```

This is a controlled nonzero process exit, not `SIGKILL` or `abort`. Later fault-injection tasks may
terminate it externally when validating supervisor behavior under uncooperative process death.
