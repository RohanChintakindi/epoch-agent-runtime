# Epoch: Capability-Aware Time Travel for AI Agents

Status: Draft for implementation  
Version: 0.1.0  
Date: 2026-07-15  
Target build window: Four weeks  
Relationship to ClawShell: Standalone research prototype; optional ClawShell integration

## 1. Executive summary

Epoch is a Linux runtime prototype for long-running, high-permission AI agents. It launches an
agent inside an isolated execution boundary, records a typed execution history, creates
composite checkpoints, forks logical branches, compares states semantically, and restores an
execution without silently replaying irreversible actions or restoring revoked authority.

The project tests one central hypothesis:

> Agent recovery is correct only when rollbackable execution state is coordinated with a
> monotonic control plane that owns current authority and external-effect history.

Epoch is not intended to become a production agent platform in this build window. It is an
experimental system designed to generate evidence about:

1. Which agent state can be captured at application, process, and filesystem levels.
2. Where CRIU-based transparent restoration succeeds or breaks.
3. How checkpoint cost changes with dirty memory, process count, workspace size, and branch
   fan-out.
4. How much latency and compatibility cost a practical Linux sandbox adds.
5. Whether branch-aware capability enforcement prevents stale authority from returning after
   restore.
6. Whether an effect journal prevents duplicate irreversible actions across crashes and replay.
7. Whether a typed semantic diff explains meaningful changes better than raw file or snapshot
   diffs.

The final deliverable is a reproducible prototype, benchmark suite, failure matrix, small local
dashboard, architectural decision record, and five-minute interview demonstration.

## 2. Problem statement

Traditional process and container recovery treats the local machine as the relevant state
boundary. Agent execution crosses that boundary continuously:

- The model has a conversation and tool context.
- The process has memory, threads, file descriptors, and child processes.
- The workspace has mutable files, packages, and build artifacts.
- External services have effects such as emails, payments, database writes, and deployments.
- A policy system has current information about identity, approval, budgets, and revoked access.

Restoring only the conversation loses operating-system effects. Restoring only the process and
filesystem can recreate stale authority or repeat an external action. Restoring a whole VM still
does not restore remote systems.

Epoch models recovery across four distinct state domains:

| Domain | Examples | Rollback behavior |
|---|---|---|
| Agent context | Messages, tool state, memory records | Rollbackable |
| Local execution | Processes, memory, files, sockets | Partially rollbackable |
| Current authority | Capabilities, approvals, budgets, revocation | Monotonic; never rolled back |
| External effects | Email, payment, remote write | Monotonic or compensatable; never assumed rolled back |

The system must coordinate these domains without pretending they have identical recovery
semantics.

## 3. Goals

### G1. Composite checkpoints

Capture an epoch containing:

- Observable agent context.
- Workspace state.
- Process state where supported.
- A reference to the trusted effect frontier.
- A reference to, but not a copy of, the policy revision.

### G2. Replay and fork as separate operations

Replay must reproduce an existing branch using recorded nondeterministic boundary results.
Fork must create a new branch identity, event history, workspace layer, and capability namespace.

### G3. Safe external-effect recovery

Persist intent before dispatch, use stable operation identifiers, reconcile ambiguous outcomes,
and never silently repeat a committed irreversible effect.

### G4. Authority revalidation

A restored agent may carry an old capability handle, but every use must be checked against
trusted current policy. Restore must not reactivate an expired, consumed, revoked, or
branch-incompatible capability.

### G5. Practical Linux isolation

Run workloads with PID, mount, user, and network namespaces, cgroup v2 resource controls,
dropped Linux capabilities, and a seccomp profile. Measure overhead and compatibility.

### G6. Typed semantic state diff

Compare epochs and branches across OS, workspace, agent-context, capability, and effect state.
Normalize ephemeral identifiers so the diff emphasizes meaningful changes.

### G7. Evidence-driven architecture

Every checkpoint and isolation backend must have a benchmark, compatibility matrix, failure
analysis, and predeclared keep/narrow/kill criteria.

## 4. Non-goals

The month-one prototype will not:

- Implement a general-purpose container runtime or hypervisor.
- Guarantee instruction-level deterministic replay.
- Capture or expose hidden model chain-of-thought.
- Provide exactly-once effects when the downstream system supports neither idempotency nor
  reconciliation.
- Support arbitrary hardware devices, GPUs, every socket type, or every CRIU workload.
- Implement multi-host scheduling, live migration, or production autoscaling.
- Implement multi-tenant production authentication, billing, or enterprise RBAC.
- Merge arbitrary concurrent filesystem branches.
- Replace Kubernetes, Temporal, ClawShell, or an agent framework.
- Claim that restoring local state reverses the external world.
- Claim research novelty over related checkpoint, branching, or rollback-security systems.

## 5. Success criteria

The prototype is successful if it produces useful architectural evidence, including evidence
that narrows or kills an approach. Feature count is not a success metric.

### Functional success

1. A deterministic test agent runs inside the Linux sandbox.
2. Epoch records application, tool, process, filesystem, network, capability, and effect events.
3. The agent can create and restore an application checkpoint.
4. Supported workloads can create and restore a CRIU checkpoint.
5. A logical checkpoint can fork two branches with separate workspace layers and histories.
6. A semantic diff explains meaningful differences between two epochs or branches.
7. A branch can be suspended at an effect boundary and resumed after approval.
8. Crash injection after remote commit does not produce a duplicate effect when the downstream
   service supports idempotency.
9. A capability revoked after checkpoint remains unusable after restore.
10. The benchmark harness generates machine-readable results and a human-readable report.

### Quality success

- One command runs the deterministic demonstration on a documented Linux environment.
- Core invariants have automated tests.
- Unsupported checkpoint cases fail explicitly rather than producing a false success.
- Results include kernel, CPU, memory, runtime versions, and experiment configuration.
- The dashboard is usable for explanation but is not on the critical path for correctness.
- The repository includes design, threat model, results, limitations, and architectural decisions.

## 6. Users and use cases

### Primary user: runtime engineer

The runtime engineer wants to determine whether a checkpoint or isolation mechanism is suitable
for a particular agent workload.

Key actions:

- Run a workload under a selected sandbox and checkpoint backend.
- Inject a failure at a selected transition.
- Restore, replay, or fork an earlier epoch.
- Inspect an execution timeline and semantic diff.
- Compare performance and correctness across backends.

### Secondary user: security engineer

The security engineer wants to verify that a compromised or restored agent cannot exceed current
authority or silently duplicate an irreversible action.

### Secondary user: agent developer

The agent developer wants to inspect what changed, where two branches diverged, and why a restored
execution stopped for approval rather than continuing.

## 7. Terminology

| Term | Definition |
|---|---|
| Session | Top-level agent execution with one root lineage |
| Branch | A distinct execution lineage within a session |
| Epoch | A committed recovery point referencing agent, workspace, process, effect, and policy state |
| Replay | Reproduce an existing branch using recorded boundaries and committed-effect history |
| Fork | Start an intentionally divergent branch from an epoch |
| Effect frontier | Highest trusted effect-log position known at epoch creation |
| Capability | Opaque, scoped, branch-bound authority checked by the trusted control plane |
| Intent | Durable request to perform an external effect |
| Effect | Externally observable action that local rollback may not reverse |
| Semantic state | Normalized, typed representation of execution-relevant state |
| Promotion | Atomic selection of one branch as the canonical continuation |
| Rollback domain | State that may be restored to an earlier version |
| Trusted control plane | Host-side authority and effect state that never rolls back with the sandbox |

## 8. Threat model and trust boundary

### 8.1 Protected assets

- Real provider credentials.
- Current capability state.
- Approval decisions.
- Effect history and idempotency records.
- Integrity of checkpoint metadata.
- Host filesystem and processes outside the sandbox.
- Other sessions and branches.

### 8.2 Adversary

Assume the agent process is fully compromised and may:

- Execute arbitrary code inside its sandbox.
- Read and modify any writable sandbox state.
- Supply malicious tool arguments.
- Attempt prompt-injection-driven exfiltration.
- Retain and reuse stale capability handles.
- Crash at adversarial times.
- Fork child processes or consume excessive resources.
- Attempt to replay an already committed external action.

### 8.3 Trusted computing base

The prototype TCB includes:

- Host Linux kernel and configured isolation primitives.
- Epoch supervisor and trusted control-plane modules.
- SQLite and filesystem durability relied on for effect state.
- CRIU for process checkpoint creation and restore.
- The effect gateway and downstream idempotency contract.
- Optional ClawShell process when provider integration is enabled.

The agent, its prompts, its tools, its workspace, and its checkpointed process memory are
untrusted.

### 8.4 Required trust-boundary property

Only rollbackable state may live inside the sandbox checkpoint. Current capability decisions and
effect history must remain outside the rollback domain.

## 9. High-level architecture

```text
                    TRUSTED HOST CONTROL PLANE
┌──────────────────────────────────────────────────────────────┐
│ Session and branch manager                                   │
│ Checkpoint coordinator                                       │
│ Current policy and capability store                          │
│ Append-only event and effect journal                         │
│ Semantic-state collector and differ                          │
│ Fault injector and benchmark controller                      │
└───────────────┬────────────────────────┬─────────────────────┘
                │ lifecycle              │ capability/effect RPC
                │                        │
┌───────────────▼────────────────────────▼─────────────────────┐
│                    ENFORCEMENT BOUNDARY                       │
│ namespaces + cgroup v2 + seccomp + dropped capabilities      │
│ effect gateway + optional ClawShell provider adapter          │
└───────────────┬────────────────────────┬─────────────────────┘
                │                        │
      ┌─────────▼──────────┐    ┌────────▼───────────┐
      │ Untrusted agent     │    │ Mock external APIs │
      │ tools and workspace │    │ email/payment/etc. │
      └─────────┬──────────┘    └────────────────────┘
                │
      ┌─────────▼────────────────────────────────────┐
      │ Composite snapshot backends                  │
      │ application context + OverlayFS + CRIU       │
      └──────────────────────────────────────────────┘
```

## 10. Component specification

### 10.1 Supervisor

Responsibilities:

- Create sessions and root branches.
- Launch workloads inside the configured sandbox.
- Own the session/branch/epoch state machines.
- Coordinate safe points, checkpoint staging, commit, restore, and cleanup.
- Assign stable event sequence numbers.
- Inject configured failures.
- Expose CLI and local dashboard APIs.
- Record benchmark metadata.

The supervisor must not place provider secrets in sandbox environment variables or snapshot
metadata.

### 10.2 Sandbox manager

Sandbox modes:

1. `none`: direct process execution for baseline measurements.
2. `linux`: namespace, cgroup, capability, seccomp, and workspace isolation.
3. `strong`: optional existing gVisor or Firecracker backend used only for comparison.

The `linux` mode must support:

- User namespace with explicit UID/GID mapping where compatible.
- PID namespace.
- Mount namespace.
- Network namespace with default-deny egress and explicit test endpoints.
- cgroup v2 CPU, memory, PID, and optional I/O limits.
- No ambient capabilities and a minimal bounding set.
- Seccomp policy denying high-risk syscalls not required by the workload.
- Read-only base root filesystem and writable isolated workspace.
- Separate temporary directory.

### 10.3 Event recorder

Event sources:

- Supervisor lifecycle events.
- Agent/model/tool boundary events emitted through a small SDK or protocol.
- Effect gateway events.
- `strace`-derived process, file, and network events in the first implementation.
- `/proc` state snapshots.
- Optional eBPF inspection as a stretch experiment, not a core dependency.

Every event must have:

```json
{
  "event_id": "evt-uuid",
  "sequence": 42,
  "session_id": "ses-uuid",
  "branch_id": "br-uuid",
  "epoch_id": "ep-uuid-or-null",
  "causal_parent": "evt-uuid-or-null",
  "monotonic_ns": 123456789,
  "wall_time": "2026-07-15T12:00:00Z",
  "actor": "agent|supervisor|tool|gateway|operator",
  "kind": "tool.call",
  "input_hash": "sha256-or-null",
  "output_hash": "sha256-or-null",
  "status": "started|succeeded|failed|denied|unknown",
  "payload": {}
}
```

Large inputs and outputs should be content-addressed blobs. The event log stores hashes and
metadata, not duplicated payloads.

### 10.4 Checkpoint coordinator

The coordinator creates a composite checkpoint in stages:

1. Request a safe point from the agent when application cooperation is available.
2. Ensure no effect is in an unsafe local transition, or mark it explicitly as ambiguous.
3. Freeze or stop the relevant process tree.
4. Stage agent-context state.
5. Stage workspace state.
6. Stage process state when the CRIU backend is selected.
7. Collect semantic-state manifest.
8. Record the trusted effect frontier and current policy revision reference.
9. Validate staged components and checksums.
10. Atomically commit epoch metadata.
11. Resume or leave suspended according to the caller's request.

Partially staged checkpoints must not appear as valid epochs. Failed staging directories are
diagnostic artifacts and may be cleaned separately.

### 10.5 Checkpoint backends

#### Application backend

Captures observable agent context:

- Messages and roles.
- Model identifier and settings.
- Tool registry and versions.
- Memory records.
- Pending logical tasks.
- Recorded boundary cursor.
- User-visible summaries.

It must not claim to capture hidden model reasoning.

#### Workspace backend

Preferred implementation order:

1. OverlayFS layers on Linux.
2. Filesystem reflinks when available.
3. Full-copy fallback for control experiments only.

Each snapshot records file path, type, mode, size, content hash, executable bit, symlink target,
and relevant extended attributes where supported.

#### Process backend

Use CRIU for compatible Linux workloads. Capture:

- Process tree and threads.
- Registers and memory mappings.
- Memory pages.
- Supported file descriptors.
- Signals, timers, credentials, and namespace state supported by CRIU.

The backend must expose structured incompatibility reasons. It must not translate every restore
failure into a generic internal error.

#### Incremental process experiment

Compare full checkpoints against CRIU pre-dump or equivalent dirty-page-based incremental flow.
Measure pause time, bytes written, total restore time, chain depth, and compaction cost.

### 10.6 Semantic-state collector

The collector produces five typed sections.

#### OS state

- Normalized process tree.
- Executable identity and argument hash.
- Thread count and states.
- RSS, PSS, mapped bytes, and dirty-memory summary.
- File descriptors normalized by type and target rather than descriptor number.
- Listening sockets and outbound connections normalized by endpoint and direction.
- Namespace inode identities.
- Mounts and writable paths.
- cgroup membership and limits.
- Decoded Linux capability sets.

#### Workspace state

- Added, modified, deleted, and type-changed files.
- Package manifest and lockfile changes.
- Configuration changes for supported structured formats.
- Repository state and generated artifacts.

#### Agent state

- Observable context hash and message count.
- Model and tool configuration.
- Memory records.
- Pending logical tasks.
- Boundary cursor and recorded-output availability.

#### Authority state

- Capability requested, granted, attenuated, consumed, expired, or revoked.
- Approval requested or resolved.
- Budget allocation and consumption.
- Policy revision change.

#### Effect state

- Intent prepared, dispatched, committed, failed, unknown, or compensated.
- Duplicate request suppressed.
- Reconciliation outcome.
- Branch abandoned with pending effect.

### 10.7 Semantic differ

The differ compares two committed semantic manifests.

Normalization rules:

- Do not treat changed host PIDs alone as meaningful after restore.
- Match processes by normalized parent path, executable identity, argument hash, and occurrence.
- Do not treat file-descriptor number changes alone as meaningful.
- Suppress configured temporary files, timestamps, and known volatile runtime paths.
- Report privilege or capability changes even if no file changed.
- Report pending or committed effects at highest severity.
- Keep the machine-readable diff deterministic.
- Allow an optional model-generated explanation only as presentation, never authority.

Severity levels:

- `info`: ordinary execution detail.
- `change`: meaningful functional difference.
- `security`: authority or boundary change.
- `effect`: externally observable action.
- `risk`: ambiguity, unsupported restore, or policy conflict.

### 10.8 Capability service

Capabilities are opaque handles. The sandbox never receives the underlying provider credential or
an authoritative serialized policy object.

Capability record:

```json
{
  "capability_id": "cap-uuid",
  "session_id": "ses-uuid",
  "branch_id": "br-uuid",
  "subject": "agent-instance-id",
  "action": "email.send",
  "resource": "mailbox:test",
  "constraints": {
    "recipient_domains": ["example.test"],
    "max_uses": 1,
    "budget_units": 1
  },
  "issued_at": "timestamp",
  "expires_at": "timestamp",
  "policy_revision": 28,
  "status": "active|consumed|expired|revoked"
}
```

Rules:

- Every use is checked against current trusted state.
- Forks receive no external-effect capabilities by default.
- Delegated capabilities may only attenuate scope.
- Abandoning a branch revokes its active capabilities.
- Promoting a branch does not automatically transfer nondelegable capabilities.
- Restoring a checkpoint never changes a capability from inactive to active.
- Capability decisions emit events and appear in semantic diffs.

### 10.9 Effect gateway

All prototype irreversible tools must pass through the gateway.

Effect states:

```text
requested
   │ authorize
   ▼
prepared ──dispatch──> dispatched ──response──> committed
   │                       │                       │
   ├──denied               ├──timeout──> unknown  └──compensate──> compensated
   └──cancelled            └──error────> failed
```

Dispatch protocol:

1. Canonicalize the action and arguments.
2. Compute the input hash.
3. Validate the current branch-bound capability.
4. Persist `prepared` intent durably.
5. Dispatch with a stable operation ID when the downstream service supports idempotency.
6. Persist the result and response hash.
7. Return the recorded result to the agent.

Recovery protocol:

- If the operation is `committed`, return the recorded result without redispatch.
- If it is `dispatched`, query downstream status when supported.
- If downstream status cannot be determined, mark `unknown` and suspend for operator resolution.
- If a replayed execution generates a materially different effect at an existing replay position,
  stop and require an explicit fork or approval. Do not silently treat it as an unrelated new
  operation.
- If the effect is compensatable, compensation is a new recorded effect, not deletion of history.

The prototype may claim effectively-once behavior only for the mock downstream service that
supports idempotency. It must not claim universal exactly-once delivery.

### 10.10 Branch manager

Branch states:

```text
created -> running -> suspended -> running
                    -> completed -> promoted
                    -> abandoned
                    -> failed
```

Fork behavior:

- Allocate new branch ID.
- Reference the parent epoch.
- Create a COW workspace layer.
- Clone observable agent context.
- Restore process state into a distinct PID namespace when supported; otherwise start from the
  application checkpoint and document the fallback.
- Start a new event sequence lineage.
- Issue no external-effect capabilities by default.

Promotion behavior:

- Promotion uses an atomic compare-and-swap against the session's current canonical branch.
- Month-one promotion selects a whole branch; arbitrary merge is out of scope.
- Branches with unresolved or unknown effects cannot be promoted.
- Losing branches are abandoned and their capabilities revoked.

### 10.11 Replay engine

Recorded nondeterministic boundaries include:

- Model responses.
- Tool results.
- External API responses.
- Time and random values emitted through the test-agent SDK.

Replay modes:

- `strict`: mismatch between current boundary input and recorded input stops replay.
- `inspect`: replay recorded outputs until a selected event, then suspend.
- `fork-on-divergence`: create a new branch when the first mismatch occurs.

Instruction-level replay and arbitrary uninstrumented nondeterminism are out of scope.

### 10.12 Dashboard

The dashboard is local and read-only in month one. Required views:

- Session and branch tree.
- Event timeline with checkpoint, approval, and effect markers.
- Side-by-side semantic diff.
- Capability and effect history.
- Benchmark summary and failure matrix.

Correctness must be testable entirely through the CLI; the dashboard must not own system state.

## 11. Lifecycle semantics

### 11.1 Session lifecycle

```text
created -> starting -> running -> suspended -> running
                              -> checkpointing -> running
                              -> restoring -> running
                              -> completed
                              -> failed
```

Only one lifecycle mutation may be active for a branch at a time.

### 11.2 Safe suspension

Suspension must preserve local process and agent context while waiting for:

- Operator approval.
- Policy update.
- Effect reconciliation.
- Debug inspection.

Suspension is distinct from process termination.

### 11.3 Epoch validity

An epoch is valid only when all required snapshot components are committed and their checksums
match. Epoch metadata is immutable after commit. Derived annotations may be appended separately.

## 12. Persistent data model

SQLite is sufficient for the prototype. Use WAL mode. Effect-path transactions should use the
strongest practical synchronous setting for the demonstration environment.

Core tables:

- `sessions`
- `branches`
- `epochs`
- `snapshot_components`
- `events`
- `blobs`
- `capabilities`
- `approvals`
- `effect_intents`
- `effect_attempts`
- `semantic_manifests`
- `semantic_diffs`
- `benchmark_runs`
- `fault_injections`

Large checkpoint images and blobs live on the filesystem under content-addressed paths. SQLite
stores metadata, ownership, hashes, and references.

Suggested local layout:

```text
.epoch/
├── state.db
├── blobs/sha256/
├── sessions/<session-id>/
│   ├── branches/<branch-id>/
│   ├── epochs/<epoch-id>/
│   ├── workspaces/
│   └── traces/
├── benchmarks/
└── logs/
```

## 13. CLI specification

```text
epoch init
epoch doctor
epoch run --manifest workload.toml
epoch status <session>
epoch events <session> [--branch <branch>]

epoch checkpoint <session> [--branch <branch>] [--label <label>]
epoch restore <epoch> [--mode strict|inspect|fork-on-divergence]
epoch fork <epoch> --name <name>
epoch suspend <branch>
epoch resume <branch>
epoch branch promote <branch>
epoch branch abandon <branch>

epoch diff <epoch-or-branch> <epoch-or-branch> [--json]
epoch capability grant <branch> <action> [constraints]
epoch capability revoke <capability>
epoch effects list <session>
epoch effects resolve <effect> --committed|--failed|--compensate

epoch bench run <suite>
epoch bench report <run>
epoch fault run <scenario>
epoch serve [--bind 127.0.0.1:PORT]
epoch demo
```

`epoch doctor` validates:

- Linux kernel and architecture.
- cgroup v2 availability.
- Required namespace support.
- CRIU installation and feature check.
- OverlayFS or reflink support.
- Required privileges.
- Optional KVM/gVisor availability.

## 14. Workload manifest

Example:

```toml
name = "mock-coding-agent"
command = ["python3", "workloads/agent.py"]
workspace = "fixtures/repository"

[sandbox]
mode = "linux"
memory_max_mb = 1024
cpu_max_percent = 200
pids_max = 128
network_default = "deny"
allowed_endpoints = ["127.0.0.1:8081", "127.0.0.1:8082"]

[checkpoint]
application = true
workspace = "overlayfs"
process = "criu"

[trace]
syscalls = true
agent_boundaries = true

[faults]
enabled = true

[[capabilities]]
action = "repository.write"
resource = "workspace"

[[effects]]
action = "email.send"
gateway = "http://127.0.0.1:8081"
```

Secrets are forbidden in manifests and checkpoint metadata.

## 15. Functional requirements

### Execution

- FR-001: The supervisor shall create a stable session and root branch identity.
- FR-002: The supervisor shall launch a workload in `none` and `linux` sandbox modes.
- FR-003: The runtime shall persist lifecycle transitions before reporting them as complete.
- FR-004: The runtime shall enforce configured cgroup resource limits in `linux` mode.
- FR-005: The runtime shall expose explicit unsupported-platform errors outside Linux.

### Checkpointing

- FR-010: The runtime shall create application checkpoints for the deterministic test agent.
- FR-011: The runtime shall create workspace checkpoints using the selected backend.
- FR-012: The runtime shall attempt CRIU checkpoints and preserve structured diagnostics.
- FR-013: Composite epoch commit shall be atomic from the metadata consumer's perspective.
- FR-014: Restore shall verify component checksums before use.
- FR-015: Restore shall not modify current capability or effect history.

### Tracing and replay

- FR-020: Events shall form a stable per-branch sequence.
- FR-021: Events shall retain causal-parent relationships where known.
- FR-022: Replay shall support recorded model and tool boundaries.
- FR-023: Strict replay shall stop on input mismatch.
- FR-024: Fork-on-divergence shall create a new branch rather than mutate history.

### Capabilities and effects

- FR-030: Every effect request shall require current capability validation.
- FR-031: Capabilities shall be bound to a session and branch.
- FR-032: Restore shall not reactivate inactive capabilities.
- FR-033: Effect intent shall be durable before dispatch.
- FR-034: Committed idempotent effects shall not be redispatched during replay.
- FR-035: Ambiguous non-reconcilable effects shall suspend the branch.
- FR-036: Abandoning a branch shall revoke its active capabilities.

### Semantic state

- FR-040: The runtime shall collect a typed semantic manifest for committed epochs.
- FR-041: Diff output shall normalize PIDs and file-descriptor numbers.
- FR-042: Capability and effect changes shall be present in diff output.
- FR-043: Machine-readable diff output shall be deterministic for identical manifests.

### Experimentation

- FR-050: Benchmark runs shall record hardware, kernel, configuration, and code revision.
- FR-051: Fault scenarios shall be reproducible from a declared seed where applicable.
- FR-052: Backend decisions shall be recorded as keep, narrow, or kill with supporting results.

## 16. Non-functional requirements

- NFR-001: The prototype shall favor explicit failure over silent fallback for security and
  effect correctness.
- NFR-002: The trusted control-plane modules shall not parse natural-language policy.
- NFR-003: The dashboard shall bind to loopback by default.
- NFR-004: Checkpoint and effect metadata shall include integrity hashes.
- NFR-005: Event collection overhead shall be measured rather than assumed negligible.
- NFR-006: Benchmark results shall distinguish tracing-on and tracing-off configurations.
- NFR-007: All supported demo paths shall run without real financial, email, or production APIs.
- NFR-008: A failed experiment shall retain enough diagnostics to explain the failure.

## 17. Required experiments

### E1. COW memory scaling

Workload:

- Allocate 128 MiB, 512 MiB, 1 GiB, and optionally 2 GiB.
- Fork 1, 2, 4, and 8 children.
- Dirty 0%, 1%, 10%, 50%, and 100% of private pages.

Measure:

- Fork latency.
- Minor page faults.
- RSS and PSS.
- Total physical-memory amplification.
- Completion time.
- Comparison with explicit full copying.

### E2. Checkpoint compatibility matrix

Scenarios:

- Idle single process.
- Multithreaded process.
- Parent with children.
- Open regular files and preserved offsets.
- Pipes and Unix sockets.
- Active TCP connection.
- Deleted or externally modified backing file.
- Timers and signals.
- Increasing memory size and dirty ratio.

Output:

- Supported, unsupported, flaky, or requires cooperation.
- Dump and restore timings.
- Snapshot size.
- Failure explanation.

### E3. Checkpoint policy comparison

Policies:

- Every agent turn.
- Fixed interval.
- Before high-impact tools.
- After recovery-relevant OS mutations.
- Hybrid semantic policy.

Measure:

- Number of checkpoints.
- Total bytes written.
- Lost work after injected crash.
- Recovery correctness.
- End-to-end overhead.

### E4. Isolation boundary comparison

Compare direct process, Epoch Linux sandbox, and one optional stronger backend.

Workloads:

- Syscall-heavy loop.
- Filesystem tree traversal.
- Package installation.
- Repository build and test.
- Process fan-out.
- Network request loop.

Measure startup, warm-run latency, memory, CPU, compatibility, and checkpoint interaction.

### E5. Action-replay recovery

Inject crashes:

- Before intent persistence.
- After intent persistence, before dispatch.
- After dispatch, before remote commit.
- After remote commit, before local result persistence.
- After local commit, before response to agent.

Expected result:

- No duplicate committed action for an idempotent downstream service.
- Ambiguous unsupported cases become `unknown` and suspend.

### E6. Authority-resurrection recovery

Procedure:

1. Grant a branch an expiring one-use effect capability.
2. Create a checkpoint containing the opaque handle.
3. Consume or revoke the capability.
4. Restore the old checkpoint.
5. Attempt reuse.

Expected result: denial using current trusted state.

### E7. Semantic-diff stability

Restore an equivalent execution with different host PIDs and descriptor numbers. Confirm the diff
does not report those changes alone. Then introduce real process, connection, capability,
workspace, context, and effect changes and confirm they are reported.

## 18. Preliminary decision thresholds

These thresholds are experiment gates, not product claims. They may be revised only before the
final benchmark run, with the reason recorded.

### Process checkpoint backend

Narrow or kill transparent CRIU use for a workload class if:

- Restore correctness is below 100% in the declared supported matrix.
- A common required resource cannot be restored or explicitly externalized.
- p95 checkpoint pause exceeds 1 second for the representative workload.
- p95 restore exceeds 3 seconds without an identified optimization path.
- Incremental mode increases operational complexity without materially reducing written bytes or
  pause time.

### Isolation backend

Narrow or kill a backend if:

- It breaks required agent tools without a bounded compatibility fix.
- Median shell-heavy workload overhead exceeds 20% without a security requirement that justifies
  the cost.
- Startup latency makes the target interactive workflow unusable.
- The effective TCB is materially larger than the protection gained.

### Semantic checkpoint policy

Kill the adaptive policy if it misses any recovery-relevant mutation in the supported test corpus
or fails to reduce checkpoint traffic meaningfully relative to every-turn checkpointing.

### Effect recovery

The demonstration fails acceptance if any of 100 deterministic crash-injection runs produces a
duplicate committed idempotent effect or reactivates revoked authority.

## 19. Fault-injection plan

Fault points must have stable symbolic names and be selectable from the CLI.

Required classes:

- Supervisor termination.
- Agent termination.
- Gateway termination.
- SQLite transaction interruption.
- Checkpoint component failure.
- Checksum mismatch.
- Downstream timeout.
- Downstream commit with lost response.
- Capability revoked during suspension.
- Branch abandoned during pending effect.
- Workspace file modified outside expected lineage.

Property-style invariants:

1. Restore never increases authority.
2. One operation ID commits at most one mock external effect.
3. Event and effect histories never move backward.
4. An invalid composite checkpoint is never exposed as committed.
5. Replay cannot silently become a fork.
6. A fork cannot silently inherit nondelegable authority.
7. An unresolved effect prevents branch promotion.

## 20. Test strategy

### Unit tests

- State-machine transitions.
- Capability attenuation and expiry.
- Effect-state transitions.
- Canonical argument hashing.
- Semantic normalization and diff classification.
- Epoch checksum validation.

### Integration tests

- Supervisor with deterministic agent.
- Application checkpoint and restore.
- Workspace fork and diff.
- CRIU supported scenarios on Linux CI or a dedicated test runner.
- Gateway crash-recovery sequence.
- Revocation after checkpoint.
- Branch promotion and abandonment.

### End-to-end tests

- Full deterministic demo.
- Action replay scenario.
- Authority resurrection scenario.
- Branch divergence and semantic comparison.
- Benchmark smoke suite.

### Security tests

- Attempt access outside workspace.
- Attempt prohibited syscall.
- Attempt excessive process creation.
- Attempt unapproved network egress.
- Attempt capability reuse from another branch.
- Modify sandbox-local checkpoint data and verify integrity failure.

## 21. Implementation structure

Recommended standalone repository layout:

```text
epoch/
├── crates/
│   ├── epoch-cli/
│   ├── epoch-supervisor/
│   ├── epoch-sandbox/
│   ├── epoch-checkpoint/
│   ├── epoch-events/
│   ├── epoch-capabilities/
│   ├── epoch-effects/
│   ├── epoch-semantic-state/
│   └── epoch-dashboard/
├── workloads/
│   ├── deterministic-agent/
│   ├── memory-cow/
│   ├── process-tree/
│   ├── network-client/
│   └── repository-task/
├── mock-services/
│   ├── email/
│   └── payments/
├── benchmarks/
├── fault-tests/
├── fixtures/
├── docs/
│   ├── DESIGN.md
│   ├── THREAT_MODEL.md
│   ├── RESULTS.md
│   ├── LIMITATIONS.md
│   └── adr/
├── demo.sh
└── README.md
```

Recommended implementation choices:

- Rust for supervisor, gateway, state model, CLI, and dashboard server.
- Tokio for asynchronous control-plane I/O.
- SQLite for trusted metadata and journals.
- JSONL/content-addressed blobs for portable event artifacts.
- C or Rust for memory/process microbenchmarks.
- Python for the deterministic agent if that reduces iteration time.
- `strace` initially; eBPF only if core acceptance criteria are already met.
- Axum plus minimal embedded HTML for the dashboard.

## 22. Four-week implementation plan

### Week 1: Execution and observability

Deliverables:

- Finalized hypotheses and thresholds.
- Repository and Linux environment.
- Session, branch, epoch, and event schemas.
- Supervisor lifecycle.
- Deterministic agent and mock services.
- `none` and initial `linux` sandbox modes.
- `/proc` semantic-state collector.
- Normalized `strace` event pipeline.
- Baseline benchmark results.

Exit criterion: One command runs an isolated deterministic workload and produces a typed timeline
and semantic state manifest.

### Week 2: Checkpointing and branching

Deliverables:

- Application checkpoint backend.
- Workspace snapshot backend.
- CRIU process backend.
- Composite epoch staging and atomic commit.
- Restore flow.
- Logical fork, branch lifecycle, and promotion.
- COW memory benchmark.
- Checkpoint compatibility matrix.

Exit criterion: Supported workloads can checkpoint, restore, fork, and produce branch-specific
state without corrupting history.

### Week 3: Capabilities, effects, replay, and diff

Deliverables:

- Capability service.
- Effect gateway and mock downstream idempotency.
- Suspension and approval lifecycle.
- Recorded-boundary replay.
- Action-replay and authority-resurrection fault tests.
- Semantic differ across all five state domains.
- Adaptive checkpoint policy experiment.

Exit criterion: Crash recovery does not duplicate supported effects or reactivate revoked
authority, and branch divergence is explainable through deterministic semantic diff.

### Week 4: Evaluation and presentation

Deliverables:

- Isolation comparison.
- Full benchmark and fault-injection matrix.
- Keep/narrow/kill decisions for each experimental mechanism.
- Read-only local dashboard.
- Optional ClawShell provider adapter.
- Documentation, diagrams, demo recording, and interview walkthrough.
- Feature freeze at least three days before the interview.

Exit criterion: A new reviewer can reproduce the demo and understand the architecture, results,
failures, limitations, and recommendation without verbal context.

## 23. Demonstration script

The final five-minute demonstration should be deterministic.

1. Start a sandboxed agent modifying a fixture repository.
2. Show the execution timeline and current semantic state.
3. Create epoch `E1`.
4. Fork `safe` and `aggressive` branches.
5. Let the branches modify different files and request different capabilities.
6. Show a semantic branch diff.
7. Grant `safe` a one-use `email.send` capability.
8. Dispatch the email and inject a crash after downstream commit but before local response.
9. Restore `E1` and demonstrate suppression/reconciliation of the repeated action.
10. Revoke the old capability and demonstrate that restoring its handle does not restore its
    authority.
11. Show checkpoint, restore, snapshot-size, and isolation measurements.
12. End with one architecture kept, one narrowed, and one killed based on evidence.

## 24. Expected final recommendation shape

The project should be able to support or reject a conclusion resembling:

> Use a hybrid recovery model. Application checkpoints provide portable semantic context;
> process checkpoints preserve expensive live local execution for compatible workloads;
> COW workspace layers support cheap branch exploration; and a monotonic control plane outside
> the rollback domain owns capabilities and external effects. Select checkpoint granularity using
> observed recovery-relevant changes rather than checkpointing every turn. Treat unsupported or
> ambiguous external resources as suspension boundaries, not transparent-success cases.

The actual recommendation must follow the results rather than being predetermined.

## 25. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| CRIU environment setup consumes excessive time | Core schedule slips | Make app/workspace checkpoint path independently complete; time-box CRIU setup |
| Branching live process state is unreliable | Demo instability | Support application-level branch fallback and document process-backend limitation |
| `strace` overhead distorts benchmarks | Misleading results | Measure tracing on/off; exclude tracing from backend baseline where appropriate |
| OverlayFS requires privileges/configuration | Portability issues | Add reflink and full-copy control backends |
| Scope expands into full platform | Incomplete result | Enforce non-goals and weekly exit criteria |
| Dashboard consumes too much time | Correctness work displaced | Keep UI read-only and build only in week four |
| Real LLM introduces nondeterminism and cost | Tests become flaky | Use recorded outputs and deterministic agent for acceptance tests |
| Effect gateway overclaims exactly-once | Incorrect architecture | Require downstream idempotency/reconciliation; suspend unknown outcomes |
| Semantic diff becomes LLM-dependent | Non-deterministic security result | Keep typed deterministic diff authoritative |
| macOS development hides Linux behavior | Invalid systems conclusions | Run implementation and benchmarks on a dedicated Linux environment |

## 26. Related work to acknowledge

- CRIU for process checkpoint/restore.
- OverlayFS and reflink-based COW workspaces.
- Crab for semantics-aware checkpoint scheduling in agent sandboxes.
- DeltaBox for incremental process and filesystem checkpointing.
- ACRFence for action replay and authority resurrection.
- Waypoint/StateFork for branchable terminal environments.
- Firecracker and gVisor for stronger isolation tradeoffs.
- ClawShell for keeping provider credentials outside the untrusted agent process.

Epoch's purpose is to test coordination among these concerns at a product boundary, not to claim
that it independently invented their underlying mechanisms.

## 27. Open questions

These should be resolved through implementation or explicitly left open in `RESULTS.md`.

1. What is the smallest safe point at which process, workspace, and agent context form a
   consistent composite checkpoint?
2. When is retry/fork cheaper than preserving exact process state?
3. Which OS mutations reliably predict that a checkpoint is recovery-relevant?
4. Should speculative branches be forbidden from irreversible effects by default, or permitted
   with branch-specific approval?
5. How should a promoted branch inherit budgets and delegable capabilities?
6. How should event causality connect an agent tool call to the syscalls and files it caused?
7. Which socket and external-resource classes should be externalized rather than checkpointed?
8. How deep can incremental snapshot chains become before compaction is required?
9. Which semantic normalization rules hide noise without suppressing meaningful security changes?
10. What restore latency is acceptable for interactive, background, and long-running agents?

## 28. Acceptance checklist

### Architecture

- [ ] Rollback and monotonic state domains are documented and enforced.
- [ ] TCB and adversary are explicit.
- [ ] Replay and fork have separate semantics.
- [ ] Unsupported resources have explicit handling.

### Implementation

- [ ] Deterministic workload runs under Linux sandbox.
- [ ] Typed event history is persisted.
- [ ] Application checkpoint and restore work.
- [ ] Workspace snapshot and branch work.
- [ ] CRIU backend has a documented compatibility matrix.
- [ ] Composite epoch commit is atomic from metadata readers' perspective.
- [ ] Capability revalidation works after restore.
- [ ] Effect journal handles every declared crash point.
- [ ] Semantic diff covers OS, workspace, context, authority, and effects.

### Evaluation

- [ ] COW benchmark completed.
- [ ] Checkpoint policy comparison completed.
- [ ] Isolation comparison completed.
- [ ] At least 100 deterministic effect fault runs completed without duplicate commit.
- [ ] Authority-resurrection test passes.
- [ ] Keep/narrow/kill decision recorded for each backend.

### Handoff

- [ ] One-command demo.
- [ ] Reproducible Linux setup.
- [ ] Architecture diagram.
- [ ] Results and limitations.
- [ ] Backup screen recording.
- [ ] Five-minute interview walkthrough rehearsed.
