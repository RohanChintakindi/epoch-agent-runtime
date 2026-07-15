# Five-minute interview demo

This demo exercises the implemented recovery vertical slice without an LLM key, network service,
email account, or production workspace. It runs a deterministic test agent, captures durable
events and two application checkpoints, changes a workspace, restores and inspects state from
fresh CLI processes, computes a semantic diff, and creates a durable logical fork.

The command is intentionally honest about its boundary. A successful run ends as
`completed_with_unsupported`, because application context recovery is implemented while workspace
rollback, process isolation/checkpointing, capability reconciliation, and effect-frontier
reconciliation are not yet registered end to end.

## Reproduce it

From the repository root:

```bash
cargo build --locked -p epoch-cli -p epoch-test-agent
DEMO_ROOT="$(mktemp -d)"
./target/debug/epoch demo \
  --agent "$(pwd)/target/debug/epoch-test-agent" \
  --root "$DEMO_ROOT" \
  --workspace "$DEMO_ROOT/workspaces"
```

The final line identifies `report.json`, the durable machine-readable evidence bundle. For JSON on
standard output, add `--json`. Each rerun gets a new session directory beneath `runs/` and a new
workspace directory, so it cannot silently reuse earlier evidence.

Use a new empty directory, or a directory previously claimed by this command. `epoch demo` refuses
a nonempty unowned root, a symlink root, a relative path, or a workspace outside the root. It also
requires an explicit absolute executable path for the test agent. A phase failure returns nonzero,
keeps all evidence recorded up to that point, and bounds child-process diagnostics.

## Exact five-minute talk track

### 0:00–0:35 — Frame the question

“Epoch asks which pieces of a long-running agent can be captured, inspected, restored, compared,
and branched reliably. This is a deterministic systems experiment, not a claim that a live model,
CRIU, or external effects are already transparent.”

Run the command above. Point out that one command invokes fresh CLI processes for every operation.

### 0:35–1:30 — Execution and durable evidence

Point to `doctor`, `run_baseline`, `status_before_checkpoint`, and `events`.

“Doctor separates a backend that is actually registered from a host tool that merely exists. The
seeded agent emits a versioned JSONL boundary trace. Session, branch, event, and epoch identifiers
are persisted in SQLite; larger components live in verified content-addressed blobs.”

### 1:30–2:25 — Checkpoint and restore boundary

Point to `checkpoint_baseline`, `controlled_workspace_change`, `run_variant`,
`checkpoint_variant`, `restore_baseline`, and `status_after_restore`.

“The checkpoint captures normalized application context. I deliberately change a file afterward,
then restore the earlier application epoch from another process. The application context moves
back, but the file change remains. The report says that clearly instead of presenting a partial
restore as full-machine recovery.”

### 2:25–3:20 — Semantic diff

Point to `semantic_diff` and its `change_count`, before/after epoch IDs, and unsupported sections.

“A byte diff is noisy for agent state. This diff compares normalized messages, pending tasks,
tool history, and model metadata. The two seeded executions are observably different. Workspace,
capability, and effect sections remain typed unsupported boundaries until their trusted sources
are connected.”

### 3:20–4:10 — Fork and restart safety

Point to `fork` and `branch_inspect`.

“Fork creates immutable lineage from the baseline epoch. Inspection happens through another CLI
process, proving the branch is durable rather than an in-memory object. Recorded model-result
evidence is present; autonomous continuation and effect inheritance are explicitly unsupported.”

### 4:10–5:00 — Recommendation and next experiments

Point to `completed_with_unsupported` and the four unsupported sections.

“The evidence supports keeping cooperative application checkpoints as the portable baseline and
keeping semantic diff plus durable branch lineage. Next I would connect a COW workspace backend,
then benchmark a Linux isolation/process-checkpoint backend against that baseline. Capabilities
and external effects must remain in monotonic trusted state outside rollback; restore may
reconcile against them, never resurrect them.”

Do not claim that the current demo rolls back files, restores a live process, replays external
effects, or enforces branch-bound authority. The report is designed to make those overclaims hard.

## Evidence and failure drill

The default summary is optimized for a live walkthrough. Use JSON when discussing the contract:

```bash
./target/debug/epoch demo \
  --agent "$(pwd)/target/debug/epoch-test-agent" \
  --root "$DEMO_ROOT" \
  --workspace "$DEMO_ROOT/workspaces" \
  --json
```

The top-level outcome, ordered phases, per-phase duration, reduced evidence identifiers, explicit
unsupported sections, and optional failure are stable report fields. The full child execution
history remains in the run's normal Epoch database and blob store.

To demonstrate bounded failure handling safely, pass an executable fixture that exits nonzero.
The command records `doctor` as succeeded, `run_baseline` as failed, writes the partial report, and
returns exit code 125. It never converts a failed child into a successful demo.

## Integration hooks

The current seams for the next experiments are deliberately narrow:

| Section | Present evidence | Hook required for support | Acceptance gate |
|---|---|---|---|
| Workspace | Controlled mutation remains observable after application restore | Register a workspace snapshot backend with immutable layer ID and manifest hash in the composite epoch | Restored files, modes, and symlinks match the checkpoint while the parent layer stays immutable |
| Isolation | `doctor` identifies direct execution as registered and reports other backends separately | Add a registered Linux launcher behind the supervisor, returning namespace, cgroup, and seccomp evidence | Escape tests fail closed and startup/runtime overhead is benchmarked |
| Process | Process observation exists; no process checkpoint backend is registered | Connect a versioned process-checkpoint backend and compatibility report to composite checkpoint/restore | Supported workloads restore; incompatible kernel state returns typed unsupported, never false success |
| Capabilities | Stable capability IDs exist, but branch authority is not reconciled | Store grants and revocations in monotonic trusted state and invoke an authority reconciler during restore/fork | Restoring an old handle cannot restore revoked authority |
| Effects | Durable event history and an explicit fork frontier boundary exist | Add idempotency records plus downstream reconciliation outside the rollback domain | Crash-after-commit retries suppress or reconcile duplicates deterministically |
| Continuation | Recorded-result provenance is inspectable | Register a resumable agent adapter with an explicit context/schema contract | A child continues from its fork point without inheriting unapproved authority or effects |

An integration should change a section from `unsupported` only after its evidence is part of the
atomic epoch contract and its failure modes are tested. Tool detection alone is insufficient.

## Pre-interview rehearsal

- Run the demo three times from the intended laptop and keep the three reports.
- Rehearse once with networking disabled; the deterministic path should still work.
- Keep a terminal sized so all 13 phase names fit without wrapping.
- Know the distinction between application restore, workspace restore, and process restore.
- End on the architecture decision supported by the evidence, not on a feature checklist.
