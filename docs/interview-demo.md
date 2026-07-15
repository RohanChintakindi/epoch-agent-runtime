# Five-minute interview demo

This demo exercises the implemented recovery vertical slice without an LLM key, network service,
email account, or production workspace. It runs a deterministic test agent, captures durable
events and two application checkpoints, changes a workspace, restores and inspects state from
fresh CLI processes, computes a semantic diff, and creates a durable logical fork.

The command is intentionally honest about its boundary. A successful run ends as
`completed_with_unsupported`: application context and workspace restore, monotonic capability
state, effect frontiers, semantic diff, and fork lineage are implemented. Live process-memory
restore, the Linux-to-supervisor launch adapter, autonomous branch continuation, and live-provider
effect reconciliation remain explicit narrow boundaries.

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

“The checkpoint captures normalized application context and an immutable full-copy workspace
manifest. I deliberately change a file afterward, then restore the earlier composite epoch from
another process into a clean no-clobber target. The original workspace remains visibly changed,
while the restored target reproduces the checkpointed bytes. Live process memory is not claimed.”

### 2:25–3:20 — Semantic diff

Point to `semantic_diff` and its `change_count`, before/after epoch IDs, and unsupported sections.

“A byte diff is noisy for agent state. This diff compares normalized messages, pending tasks,
tool history, model metadata, workspace manifests, and monotonic capability/effect frontiers. The
two seeded executions are observably different without treating rolled-back sandbox bytes as
trusted authority.”

### 3:20–4:10 — Fork and restart safety

Point to `fork` and `branch_inspect`.

“Fork creates immutable lineage from the baseline epoch. Inspection happens through another CLI
process, proving the branch is durable rather than an in-memory object. Security frontiers remain
outside rollback. Recorded-result inspection works; autonomous continuation and live-provider
reconciliation remain explicit.”

### 4:10–5:00 — Recommendation and next experiments

Point to `completed_with_unsupported` and the four unsupported sections.

“The evidence supports keeping cooperative application plus immutable workspace checkpoints as
the portable baseline, and keeping semantic diff, monotonic authority, effect journaling, and
durable branch lineage. The next product bet is a measured Linux supervisor adapter and a narrowly
compatible process-checkpoint backend—not a claim that arbitrary processes are transparent.”

Do not claim that the demo overwrites the original workspace, restores a live process, continues a
fork autonomously, or proves exactly-once behavior for a live external provider. The report is
designed to make those overclaims hard.

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
| Workspace | Composite checkpoints publish verified files, modes, and symlinks to a clean target while preserving the mutated original | Keep the full-copy backend as baseline; evaluate COW only as a measured optimization | Restored manifest matches and no-clobber remains enforced |
| Isolation | `doctor` identifies direct execution as registered and reports other backends separately | Add a registered Linux launcher behind the supervisor, returning namespace, cgroup, and seccomp evidence | Escape tests fail closed and startup/runtime overhead is benchmarked |
| Process | Process observation exists; no process checkpoint backend is registered | Connect a versioned process-checkpoint backend and compatibility report to composite checkpoint/restore | Supported workloads restore; incompatible kernel state returns typed unsupported, never false success |
| Capabilities | Branch-bound grants, consumption, revocation, policy revisions, and audit are monotonic trusted state | Keep authority outside rollback and bind every effect intent atomically | Restoring an old handle cannot restore revoked or stale authority |
| Effects | Stable operation IDs, durable transitions, duplicate suppression, unknown outcomes, branch suspension, and checkpoint frontiers are implemented | Add provider-specific lookup/reconciliation behind the trusted dispatcher | 100 local replays dispatch once; ambiguous provider commits remain unknown until reconciled |
| Continuation | Recorded-result provenance is inspectable | Register a resumable agent adapter with an explicit context/schema contract | A child continues from its fork point without inheriting unapproved authority or effects |

An integration should change a section from `unsupported` only after its evidence is part of the
atomic epoch contract and its failure modes are tested. Tool detection alone is insufficient.

## Pre-interview rehearsal

- Run the demo three times from the intended laptop and keep the three revision-pinned reports plus
  their SHA-256 checksums.
- Rehearse once with networking disabled; the deterministic path should still work.
- Keep a terminal sized so all 13 phase names fit without wrapping.
- Know the distinction between application restore, workspace restore, and process restore.
- End on the architecture decision supported by the evidence, not on a feature checklist.
