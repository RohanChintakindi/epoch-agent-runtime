# Epoch implementation plan

Status: Ready to execute  
Date: 2026-07-15  
Source specification: [epoch-runtime-spec.md](epoch-runtime-spec.md)  
Target: One-month research prototype  
Primary implementation: Rust control plane; Python/C test workloads; Linux execution host

## 1. Planning principles

This backlog is ordered around working vertical slices rather than component completeness.
Every milestone must leave the repository runnable and demonstrable.

Rules:

1. Every behavior change follows red → green → refactor; bug fixes begin with a failing
   regression test.
2. A task is not complete until its acceptance checks pass.
3. Security-sensitive fallbacks must be explicit in events and CLI output.
4. Control-plane correctness comes before UI work.
5. Deterministic workloads are required before a live LLM integration.
6. Application and workspace checkpoints must work independently of CRIU.
7. Failed experiments are deliverables when they include reproducible evidence.
8. New scope enters the backlog as P1 or P2; it does not silently expand P0.
9. Benchmark thresholds are recorded before final benchmark collection.

Priority definitions:

- **P0:** Required for the final interview demonstration.
- **P1:** Important role-aligned experiment; implement after its P0 dependency is stable.
- **P2:** Stretch work only after the feature freeze candidate works end to end.

Estimate definitions:

- **0.5d:** A focused half-day task.
- **1d:** One full implementation and verification day.
- **2d:** Maximum acceptable task size; split further if it exceeds two days.

## 2. Milestones

| Milestone | Outcome | Target |
|---|---|---|
| M0: Buildable foundation | Repository, Linux environment, CI, domain model | End of day 2 |
| M1: Observable execution | Run a deterministic agent and inspect its event/state history | End of week 1 |
| M2: Recoverable execution | Create and restore composite application/workspace/process epochs | Mid week 2 |
| M3: Branchable execution | Fork, replay, compare, promote, and abandon branches | End of week 2 |
| M4: Effect-safe recovery | Prevent action replay and authority resurrection | End of week 3 |
| M5: Evidence-backed prototype | Benchmarks, fault matrix, dashboard, docs, rehearsed demo | End of week 4 |

## 3. Critical path

```text
B01 Repository scaffold
  -> C01 Domain model
  -> C02 SQLite migrations
  -> C04 Event journal
  -> W04 Direct supervisor
  -> K01 Checkpoint interfaces
  -> K05 Composite epoch commit
  -> K06 Restore coordinator
  -> R01 Branch state machine
  -> R02 Fork coordinator
  -> A01 Capability service
  -> A02 Effect journal
  -> A05 Crash injection
  -> A06 Action-replay tests
  -> A07 Authority-resurrection tests
  -> S04 Semantic diff
  -> Q01 End-to-end acceptance suite
  -> Q05 Final demo
```

Parallel work that must not block the critical path:

- Linux sandbox implementation can progress after W04.
- CRIU compatibility work can progress after K01 while application/workspace restore is built.
- COW benchmarks can progress once the Linux environment exists.
- Dashboard work starts only after event and diff query APIs are stable.

## 4. Phase B: Bootstrap and environment

### B01 — Create standalone Epoch workspace

- Priority: P0
- Estimate: 0.5d
- Dependencies: none
- Deliverable:
  - `epoch/` Cargo workspace.
  - Initial crates for CLI, core domain, and storage.
  - Formatting, lint, test, and local run commands.
- Acceptance:
  - `cargo fmt --check` passes.
  - `cargo clippy --all-targets -- -D warnings` passes.
  - `cargo test --workspace` passes.
  - `epoch --help` runs.

### B02 — Provision documented Linux execution environment

- Priority: P0
- Estimate: 1d
- Dependencies: B01
- Deliverable:
  - Dedicated Linux x86_64 or arm64 environment with root access.
  - Rust toolchain, CRIU, `strace`, `perf`, cgroup v2, OverlayFS, and SQLite.
  - `scripts/doctor.sh` or equivalent diagnostic output.
- Acceptance:
  - Kernel and tool versions are captured.
  - CRIU feature check output is stored.
  - User/PID/mount/network namespaces are verified.
  - A cgroup v2 test limit is applied successfully.
  - Known macOS limitations are documented.

### B03 — Add Linux CI and local quality commands

- Priority: P0
- Estimate: 0.5d
- Dependencies: B01
- Deliverable:
  - Linux CI for format, clippy, unit tests, and nonprivileged integration tests.
  - Separate label or script for privileged CRIU/isolation tests.
- Acceptance:
  - Ordinary CI does not silently skip failed unit tests.
  - Privileged tests report `not available` distinctly from `passed`.

### B04 — Record initial architectural decisions

- Priority: P0
- Estimate: 0.5d
- Dependencies: B01
- Deliverable:
  - ADR-001: trusted state outside rollback domain.
  - ADR-002: SQLite metadata plus content-addressed blobs.
  - ADR-003: deterministic workload before real LLM.
  - ADR-004: application/workspace fallback independent of CRIU.
- Acceptance:
  - Each ADR includes context, decision, alternatives, and consequences.

## 5. Phase C: Core domain and durable storage

### C01 — Implement domain identifiers and state machines

- Priority: P0
- Estimate: 1d
- Dependencies: B01
- Deliverable:
  - Typed IDs for session, branch, epoch, event, capability, and effect.
  - Session and branch lifecycle enums.
  - Validated transition functions.
- Acceptance:
  - Invalid transitions return typed errors.
  - Unit tests cover every allowed and denied transition.
  - Serialization is stable and versioned.

### C02 — Implement SQLite migrations and connection layer

- Priority: P0
- Estimate: 1d
- Dependencies: C01
- Deliverable:
  - Migrations for sessions, branches, epochs, events, blobs, capabilities, approvals,
    effect intents/attempts, semantic manifests/diffs, benchmarks, and fault injections.
  - WAL and synchronous-mode configuration.
- Acceptance:
  - Fresh database migrates to latest schema.
  - Reopening the database is idempotent.
  - Migration failure leaves the previous schema usable or fails before mutation.
  - Foreign keys are enabled and tested.

### C03 — Implement content-addressed blob store

- Priority: P0
- Estimate: 0.5d
- Dependencies: C01
- Deliverable:
  - SHA-256-addressed blobs with atomic temporary-file rename.
  - Metadata for length and media type.
- Acceptance:
  - Duplicate content is stored once.
  - Hash mismatch is detected on read.
  - Interrupted writes do not expose valid-looking blobs.

### C04 — Implement append-only event journal

- Priority: P0
- Estimate: 1d
- Dependencies: C02, C03
- Deliverable:
  - Event schema from the specification.
  - Per-branch monotonic sequence allocation.
  - Causal-parent support.
  - Query by session, branch, event kind, and sequence range.
- Acceptance:
  - Concurrent append tests never duplicate a branch sequence.
  - Existing events cannot be updated through the public API.
  - Large payloads are stored by blob hash.
  - Query results have deterministic ordering.

### C05 — Implement CLI shell and `epoch doctor`

- Priority: P0
- Estimate: 1d
- Dependencies: C01, C02
- Deliverable:
  - Command tree from the spec with unfinished commands returning explicit `not implemented`.
  - Platform diagnostics for Linux, cgroup v2, namespaces, CRIU, OverlayFS/reflink, `strace`,
    `perf`, KVM, and gVisor.
- Acceptance:
  - CLI help documents every command group.
  - `doctor --json` produces machine-readable output.
  - macOS reports control-plane-only mode rather than pretending Linux features exist.

## 6. Phase W: Deterministic workloads and observable execution

### W01 — Define agent boundary protocol

- Priority: P0
- Estimate: 0.5d
- Dependencies: C04
- Deliverable:
  - JSONL protocol for agent start, context update, model request/response, tool call/result,
    safe point, and completion.
  - Protocol version field and validation.
- Acceptance:
  - Malformed input produces a typed protocol error.
  - Unknown future fields are handled according to documented compatibility rules.

### W02 — Build deterministic test agent

- Priority: P0
- Estimate: 1d
- Dependencies: W01
- Deliverable:
  - Scripted agent capable of file writes, child processes, memory allocation, network calls,
    boundary events, safe points, and configurable crashes.
  - Recorded deterministic model responses.
- Acceptance:
  - Same seed produces the same boundary history.
  - Workload scenarios are selectable from the command line.
  - No real secrets or external services are required.

### W03 — Build idempotent mock email/payment services

- Priority: P0
- Estimate: 1d
- Dependencies: C02
- Deliverable:
  - Local mock service accepting stable operation IDs.
  - Status lookup and deterministic lost-response simulation.
  - Durable record of committed operations.
- Acceptance:
  - Repeating one operation ID returns one committed result.
  - A different payload with the same operation ID is rejected.
  - Lost-response mode commits remotely while withholding the response.

### W04 — Implement direct-process supervisor

- Priority: P0
- Estimate: 1d
- Dependencies: C01, C04, W01
- Deliverable:
  - Session and root-branch creation.
  - Workload launch, stdout/stderr capture, boundary-protocol ingestion, exit handling, and
    lifecycle event emission.
- Acceptance:
  - `epoch run` launches W02 in direct mode.
  - Lifecycle and boundary events persist through supervisor restart.
  - Nonzero agent exit is distinct from supervisor failure.

### W05 — Implement `/proc` semantic collector

- Priority: P0
- Estimate: 1.5d
- Dependencies: W04, B02
- Deliverable:
  - Linux collector for process tree, threads, status, maps summary, file descriptors,
    namespaces, cgroup, capabilities, and network endpoints where available.
  - Structured unsupported result on non-Linux.
- Acceptance:
  - Fixture tests validate parser behavior.
  - Permission-denied and process-disappeared races do not crash collection.
  - Collection output has a schema version.

### W06 — Implement `strace` normalizer

- Priority: P1
- Estimate: 1.5d
- Dependencies: W04, B02
- Deliverable:
  - `strace -ff` launcher and parser.
  - Normalized process, file, and network events.
- Acceptance:
  - Handles unfinished/resumed syscalls and child trace files.
  - Parser has fixture tests.
  - Tracing-on overhead is measured separately from tracing-off baseline.

### M1 acceptance — Observable execution

- `epoch run` executes the deterministic agent.
- CLI lists the session, branch, lifecycle, and typed event timeline.
- A `/proc` semantic manifest is stored on Linux.
- Supervisor restart does not lose committed history.

## 7. Phase I: Linux isolation

### I01 — Define sandbox backend interface

- Priority: P0
- Estimate: 0.5d
- Dependencies: W04
- Deliverable:
  - Backend trait for prepare, launch, inspect, suspend, resume, terminate, and cleanup.
  - Direct backend migrated behind the trait.
- Acceptance:
  - Direct execution behavior remains unchanged.
  - Backend capability discovery is explicit.

### I02 — Implement namespace and root-filesystem launcher

- Priority: P0
- Estimate: 2d
- Dependencies: I01, B02
- Deliverable:
  - User, PID, mount, and network namespace setup.
  - Read-only base plus writable workspace and isolated temporary directory.
- Acceptance:
  - Agent cannot see host PIDs.
  - Writes outside allowed mounts fail.
  - Namespace identities appear in semantic state.
  - Cleanup removes mounts and processes after failures.

### I03 — Implement cgroup v2 resource manager

- Priority: P0
- Estimate: 1d
- Dependencies: I01, B02
- Deliverable:
  - CPU, memory, and PID controls.
  - Usage and pressure metrics capture where available.
- Acceptance:
  - Fork-bomb fixture is stopped by `pids.max`.
  - Memory-limit fixture has a visible OOM outcome.
  - Child processes remain in the session cgroup.

### I04 — Drop capabilities and apply seccomp

- Priority: P0
- Estimate: 1.5d
- Dependencies: I02
- Deliverable:
  - Empty ambient set and minimal bounding/effective/permitted sets.
  - Versioned seccomp profile for deterministic workloads.
- Acceptance:
  - Prohibited syscall fixture is denied predictably.
  - Required workload syscalls still work.
  - Effective capability sets appear in semantic state.

### I05 — Implement explicit network policy

- Priority: P1
- Estimate: 1d
- Dependencies: I02, W03
- Deliverable:
  - Default-deny sandbox network with allowlisted mock service endpoints.
- Acceptance:
  - Mock service is reachable when allowed.
  - Unapproved endpoint fixture is blocked.
  - Policy decision is recorded as an event.

## 8. Phase K: Composite checkpoint and restore

### K01 — Define checkpoint backend interfaces

- Priority: P0
- Estimate: 1d
- Dependencies: C01, C03, W04
- Deliverable:
  - Application, workspace, process, and composite snapshot traits.
  - Structured supported/unsupported/failed outcomes.
- Acceptance:
  - A backend cannot return success without component hashes and metadata.
  - Unsupported is distinct from failure.

### K02 — Implement application checkpoint backend

- Priority: P0
- Estimate: 1d
- Dependencies: K01, W02
- Deliverable:
  - Safe-point request and observable agent-context serialization.
  - Context schema versioning and integrity validation.
- Acceptance:
  - Agent resumes with message/tool/task cursor preserved.
  - Corrupt context is rejected.
  - Hidden chain-of-thought is not represented or claimed.

### K03 — Implement workspace snapshot backend

- Priority: P0
- Estimate: 1.5d
- Dependencies: K01, I02
- Deliverable:
  - OverlayFS layer snapshot on Linux.
  - Reflink or full-copy control backend where available.
  - File manifest and content hashes.
- Acceptance:
  - Restore reproduces fixture workspace exactly.
  - Parent snapshot remains immutable after child writes.
  - Symlinks, modes, and executable bits are preserved.

### K04 — Implement CRIU process backend

- Priority: P0
- Estimate: 2d
- Dependencies: K01, B02, I02
- Deliverable:
  - CRIU feature check, dump, restore, log capture, and structured diagnostics.
  - Initial support for idle, memory, child-process, and regular-file scenarios.
- Acceptance:
  - At least the declared initial scenarios restore correctly.
  - CRIU log and failure category persist on every failure.
  - Process restore runs inside the intended isolation boundary.

### K05 — Implement atomic composite epoch commit

- Priority: P0
- Estimate: 1.5d
- Dependencies: K02, K03, C02, C03
- Deliverable:
  - Staging directory, component checksums, semantic manifest reference, effect frontier,
    policy revision reference, and atomic metadata commit.
- Acceptance:
  - Fault at every staging step never exposes a valid committed epoch.
  - Committed epoch metadata is immutable.
  - Orphaned staging artifacts are diagnosable and cleanable.

### K06 — Implement restore coordinator

- Priority: P0
- Estimate: 2d
- Dependencies: K05, K04
- Deliverable:
  - Checksum validation, workspace restore, application/process selection, lifecycle transitions,
    and current-authority reconciliation hook.
- Acceptance:
  - Application/workspace restore works without CRIU.
  - CRIU-compatible workload restores live execution.
  - Restore never modifies capability/effect tables.
  - Failure leaves branch suspended or failed with a clear reason.

### K07 — Build CRIU compatibility matrix runner

- Priority: P0
- Estimate: 1.5d
- Dependencies: K04, K06
- Deliverable:
  - Automated matrix for threads, children, files, pipes, Unix sockets, TCP, timers, signals,
    memory size, and dirty ratio.
- Acceptance:
  - Results classify supported, unsupported, flaky, or cooperation-required.
  - Every row links to logs and environment metadata.

### K08 — Prototype incremental process checkpoints

- Priority: P1
- Estimate: 2d
- Dependencies: K04, K07
- Deliverable:
  - CRIU pre-dump or equivalent experiment.
  - Chain metadata and full-versus-incremental comparison.
- Acceptance:
  - Pause time, bytes, total restore, and chain-depth costs are reported.
  - Keep/narrow/kill decision is recorded.

### M2 acceptance — Recoverable execution

- One command creates a composite epoch.
- Application and workspace restore are reliable.
- CRIU works for a documented supported subset.
- A failed component cannot create a valid-looking epoch.

## 9. Phase R: Branching and replay

### R01 — Implement branch lifecycle state machine

- Priority: P0
- Estimate: 1d
- Dependencies: C01, K05
- Deliverable:
  - Create, run, suspend, complete, promote, abandon, and fail transitions.
  - Canonical-branch reference on session.
- Acceptance:
  - Transition tests cover concurrent promotion attempts.
  - Only one canonical branch wins atomic promotion.

### R02 — Implement logical fork coordinator

- Priority: P0
- Estimate: 1.5d
- Dependencies: R01, K02, K03, K06
- Deliverable:
  - New branch identity, parent epoch, COW workspace, cloned observable context, independent
    event lineage, and empty external-effect capability set.
- Acceptance:
  - Parent snapshot remains immutable.
  - Child changes do not appear in sibling state.
  - Child has no external-effect capability unless explicitly granted.

### R03 — Implement boundary recording cache

- Priority: P0
- Estimate: 1d
- Dependencies: W01, C03, C04
- Deliverable:
  - Canonical hashes and stored results for model, tool, time, and random boundaries.
- Acceptance:
  - Identical boundary request retrieves the recorded result.
  - Same position with different canonical input is reported as divergence.

### R04 — Implement replay modes

- Priority: P0
- Estimate: 2d
- Dependencies: R03, R02
- Deliverable:
  - Strict, inspect, and fork-on-divergence behavior.
- Acceptance:
  - Strict replay stops on first mismatch.
  - Inspect suspends at selected event.
  - Fork-on-divergence creates new branch history without mutating the original.

### R05 — Implement promote and abandon cleanup

- Priority: P0
- Estimate: 1d
- Dependencies: R01, R02
- Deliverable:
  - Atomic promotion, abandonment event, capability-revocation hook, and deferred artifact cleanup.
- Acceptance:
  - Branch with unresolved effects cannot promote.
  - Losing branch remains inspectable after abandonment.

### M3 acceptance — Branchable execution

- Fork two branches from one epoch.
- Produce independent histories and workspaces.
- Replay recorded boundaries and detect divergence.
- Promote one whole branch and abandon the other.

## 10. Phase A: Capabilities and effect-safe recovery

### A01 — Implement branch-bound capability service

- Priority: P0
- Estimate: 1.5d
- Dependencies: C02, R01
- Deliverable:
  - Grant, validate, consume, expire, revoke, and attenuate operations.
  - Opaque handles returned to the sandbox.
- Acceptance:
  - Capability cannot be used by another session or branch.
  - Attenuation cannot widen scope.
  - Restored stale handle is checked against current state.
  - Every decision emits an event.

### A02 — Implement durable effect journal

- Priority: P0
- Estimate: 1.5d
- Dependencies: C02, C04, A01
- Deliverable:
  - Requested, prepared, dispatched, committed, failed, unknown, and compensated states.
  - Durable state transitions and attempt history.
- Acceptance:
  - Invalid transitions fail closed.
  - History is append-only even when current state changes.
  - Intent is durable before dispatch.

### A03 — Implement effect gateway

- Priority: P0
- Estimate: 2d
- Dependencies: A02, W03
- Deliverable:
  - Canonicalization, input hashing, capability validation, stable operation IDs, dispatch,
    result recording, and committed-result replay.
- Acceptance:
  - Duplicate committed request returns recorded result without redispatch.
  - Same operation ID with different input is rejected.
  - Gateway never logs underlying provider secrets.

### A04 — Implement suspension and approval flow

- Priority: P0
- Estimate: 1d
- Dependencies: A01, A02, R01
- Deliverable:
  - Pending approval record, branch suspension, grant/deny resolution, and resume.
- Acceptance:
  - Suspended branch preserves local state.
  - Denial cannot be bypassed by replay.
  - Approval is tied to exact canonical intent and current policy revision.

### A05 — Implement symbolic crash injector

- Priority: P0
- Estimate: 1d
- Dependencies: A02, K06
- Deliverable:
  - Stable crash points around every effect transition and checkpoint stage.
  - Fault-run metadata and reproducible scenario command.
- Acceptance:
  - Each declared fault point can be triggered deterministically.
  - Test output distinguishes expected injected failure from product failure.

### A06 — Implement action-replay recovery suite

- Priority: P0
- Estimate: 1.5d
- Dependencies: A03, A05, K06, R04
- Deliverable:
  - Crash-before/after intent, dispatch, remote commit, local commit, and response scenarios.
- Acceptance:
  - 100 deterministic runs produce no duplicate committed idempotent effect.
  - Non-reconcilable outcome becomes `unknown` and suspends.
  - Materially changed replay intent requires fork or approval.

### A07 — Implement authority-resurrection suite

- Priority: P0
- Estimate: 1d
- Dependencies: A01, K06
- Deliverable:
  - Consume/revoke/expire after checkpoint, restore old handle, attempt reuse.
- Acceptance:
  - All stale uses are denied by current control-plane state.
  - Restore does not update capability status.

### M4 acceptance — Effect-safe recovery

- Crash after remote commit and restore without duplicate effect.
- Revoke a capability after checkpoint and prove restore cannot revive it.
- Unknown external outcomes suspend rather than silently retry.

## 11. Phase S: Semantic state and diff

### S01 — Define versioned semantic manifest schema

- Priority: P0
- Estimate: 1d
- Dependencies: W05, A01, A02
- Deliverable:
  - OS, workspace, agent, authority, and effect sections.
  - Severity and evidence fields.
- Acceptance:
  - Schema round-trips through JSON.
  - Missing optional collectors are explicit, not empty-success values.

### S02 — Compose semantic manifests at epoch commit

- Priority: P0
- Estimate: 1d
- Dependencies: S01, K05
- Deliverable:
  - Collector orchestration and content-addressed manifest persistence.
- Acceptance:
  - Manifest references the exact epoch components and effect frontier.
  - Partial collector failures are represented with status/evidence.

### S03 — Implement normalization rules

- Priority: P0
- Estimate: 1.5d
- Dependencies: S01
- Deliverable:
  - Process matching without raw PID identity.
  - FD matching without descriptor-number identity.
  - Configured volatile-path and timestamp suppression.
  - Capability decoding and endpoint normalization.
- Acceptance:
  - Equivalent restored execution produces no PID/FD-only changes.
  - Real privilege, endpoint, file, and context changes remain visible.

### S04 — Implement deterministic semantic differ

- Priority: P0
- Estimate: 2d
- Dependencies: S02, S03
- Deliverable:
  - Typed additions/removals/changes with info/change/security/effect/risk severities.
  - JSON output and stable ordering.
- Acceptance:
  - Identical manifests yield an empty diff.
  - Diff output is byte-stable for identical inputs.
  - Capability/effect changes receive correct severity.

### S05 — Implement human-readable diff renderer

- Priority: P0
- Estimate: 1d
- Dependencies: S04
- Deliverable:
  - CLI summary grouped by semantic domain and severity.
- Acceptance:
  - Every human line references machine-readable evidence.
  - No LLM is required for authoritative output.

## 12. Phase E: Experiment and benchmark suite

### E01 — Implement benchmark harness

- Priority: P0
- Estimate: 1d
- Dependencies: C02, W04
- Deliverable:
  - Warmup, repetitions, seeds, environment metadata, percentiles, JSON/CSV output.
- Acceptance:
  - Same configuration is reproducible.
  - Trace-on and trace-off results are separate.

### E02 — Implement COW memory benchmark

- Priority: P0
- Estimate: 1.5d
- Dependencies: B02, E01
- Deliverable:
  - Configurable allocation, child fan-out, dirty ratio, page-fault, RSS/PSS, and full-copy
    comparison.
- Acceptance:
  - Required matrix runs automatically.
  - Results include raw samples and summary.

### E03 — Run checkpoint compatibility and scaling benchmark

- Priority: P0
- Estimate: 1d
- Dependencies: K07, E01
- Deliverable:
  - Final supported matrix and scaling plots/tables.
- Acceptance:
  - Every result links to configuration and logs.
  - Unsupported cases are not omitted.

### E04 — Implement checkpoint-policy comparison

- Priority: P1
- Estimate: 2d
- Dependencies: K06, W06, E01
- Deliverable:
  - Every-turn, interval, pre-effect, OS-mutation, and hybrid policies.
- Acceptance:
  - Compare bytes, checkpoint count, lost work, recovery correctness, and overhead.
  - Record keep/narrow/kill decision.

### E05 — Run isolation comparison

- Priority: P0
- Estimate: 1.5d
- Dependencies: I04, E01
- Deliverable:
  - Direct versus Epoch Linux sandbox results.
  - Optional gVisor or Firecracker comparison only after P0 results exist.
- Acceptance:
  - Startup, warm latency, memory, CPU, compatibility, and checkpoint interaction reported.

### E06 — Generate decision report

- Priority: P0
- Estimate: 1d
- Dependencies: E02, E03, E05, A06, A07
- Deliverable:
  - `RESULTS.md` with one keep, one narrow, and one kill decision.
- Acceptance:
  - Every conclusion cites a measured result or correctness test.
  - Limitations and environment are explicit.

## 13. Phase U: Dashboard and integrations

### U01 — Implement read-only query API

- Priority: P1
- Estimate: 1d
- Dependencies: C04, S04, E01
- Deliverable:
  - Loopback-only APIs for sessions, branch tree, events, epochs, diffs, capabilities, effects,
    and benchmark summaries.
- Acceptance:
  - API cannot mutate runtime state.
  - Pagination and deterministic ordering exist for event lists.

### U02 — Implement minimal local dashboard

- Priority: P1
- Estimate: 2d
- Dependencies: U01
- Deliverable:
  - Branch tree, timeline, semantic diff, effect/capability history, and benchmark summary.
- Acceptance:
  - Final demo can be completed through CLI if dashboard fails.
  - Security/effect events are visually distinguishable.

### U03 — Add optional ClawShell provider adapter

- Priority: P2
- Estimate: 1.5d
- Dependencies: A01, R03, M4 acceptance
- Deliverable:
  - Agent receives only a virtual provider capability.
  - Provider request/response becomes a recorded replay boundary.
- Acceptance:
  - Real provider key never enters sandbox environment, checkpoint, or event payload.
  - Deterministic tests do not require a live provider.

### U04 — Add optional live tool-using agent

- Priority: P2
- Estimate: 1.5d
- Dependencies: U03 or R03, Q01
- Deliverable:
  - Live mode using the same boundary protocol as deterministic workloads.
- Acceptance:
  - Recorded-response mode remains the default demo/test path.

## 14. Phase Q: Hardening, documentation, and demo

### Q01 — Build end-to-end acceptance suite

- Priority: P0
- Estimate: 2d
- Dependencies: K06, R05, A06, A07, S05
- Deliverable:
  - Observable execution, checkpoint/restore, fork/replay, semantic diff, action replay, and
    authority resurrection scenarios.
- Acceptance:
  - One command runs the deterministic suite on the documented Linux host.
  - Failure output identifies the violated invariant.

### Q02 — Build sandbox security suite

- Priority: P0
- Estimate: 1d
- Dependencies: I04, I05
- Deliverable:
  - Host path, prohibited syscall, fork bomb, memory abuse, unapproved egress, cross-branch
    capability reuse, and checkpoint tampering tests.
- Acceptance:
  - Every attack is blocked or explicitly documented as an unsupported boundary.

### Q03 — Finish project documentation

- Priority: P0
- Estimate: 1.5d
- Dependencies: E06, Q01
- Deliverable:
  - README, design, threat model, results, limitations, reproduction instructions, ADRs, and
    related-work acknowledgements.
- Acceptance:
  - New reviewer can reproduce the deterministic demo without verbal help.
  - No unmeasured performance or exactly-once claims remain.

### Q04 — Create demo assets

- Priority: P0
- Estimate: 1d
- Dependencies: Q01, U02 optional
- Deliverable:
  - `demo.sh`, fixture task, expected checkpoints, backup recording, and five-slide walkthrough.
- Acceptance:
  - Demo runs in five minutes from a clean prepared environment.
  - Backup recording shows the same commit and result set.

### Q05 — Feature freeze and interview rehearsal

- Priority: P0
- Estimate: 1d plus buffer
- Dependencies: Q03, Q04
- Deliverable:
  - Frozen demo commit, known-issues list, technical Q&A notes, and practiced five-minute talk.
- Acceptance:
  - Explain one mechanism kept, one narrowed, and one killed.
  - Explain why sandbox rollback cannot roll back the control plane or outside world.
  - Complete demo three consecutive times without manual repair.

### M5 acceptance — Evidence-backed prototype

- Full deterministic demo passes.
- Results and failure matrix are reproducible.
- Security invariants hold under declared fault tests.
- Dashboard is optional; CLI is sufficient.
- Project is frozen several days before the interview.

## 15. Product gates and scope protection

The project must become a usable vertical product before the final two weeks. Calendar progress
alone does not advance a gate.

### End-of-Week-1 gate — Observable execution

- `epoch run` launches the deterministic agent through the direct supervisor.
- Session, root branch, lifecycle, stdout/stderr, and typed boundary events are durable.
- `epoch status` and `epoch events` remain useful after the supervisor restarts.
- macOS control-plane tests and Linux CI pass; the dedicated Linux host is ready for privileged
  validation.

### End-of-Week-2 gate — Usable recovery product

- `epoch run`, `status`, `events`, `checkpoint`, `restore`, and `diff` work from the CLI.
- Application context and workspace checkpoints work without CRIU; CRIU is an additional backend,
  not the only product path.
- A corrupted or partial checkpoint is rejected before restore.
- At least one isolation backend and the direct backend expose explicit capability discovery.
- The deterministic run-checkpoint-mutate-restore-inspect flow completes three times without manual
  database or filesystem repair.

If this gate is not met, Week 3 branching, replay, and capability breadth is narrowed until the
recovery product is complete. Dashboard work, live LLM integration, Firecracker, and visual polish
cannot displace this gate.

### End-of-Week-3 gate — Security and branching depth

- Fork/replay behavior is demonstrable from a committed epoch.
- Restoring old execution state cannot restore revoked authority or erase committed effect history.
- Semantic diff explains workspace, context, capability, and effect changes.
- Linux isolation and checkpoint limits are measured and documented with structured unsupported
  cases.

### Week 4 rule — Freeze, evidence, and rehearsal

No interview-critical product feature is scheduled to first appear in Week 4. Week 4 is reserved
for fault tests, benchmarks, documentation, scope cuts, compatibility evidence, and repeated demo
rehearsal. A late feature may enter only if all earlier gates remain green.

## 16. Suggested four-week schedule

### Week 1

- Days 1–2: B01–B04, C01–C02.
- Days 3–4: C03–C05, W01–W04.
- Day 5: W05, I01, M1 acceptance.

### Week 2

- Days 6–7: I02–I04 and K01–K03.
- Days 8–9: K04–K06.
- Day 10: K07, R01–R02, M2 acceptance.

### Week 3

- Days 11–12: R03–R05, M3 acceptance.
- Days 13–14: A01–A04.
- Day 15: A05–A07, M4 acceptance.

### Week 4

- Days 16–17: S01–S05.
- Days 18–19: E01–E06 and Q01.
- Day 20: Q02–Q04.
- Remaining calendar buffer: U01–U02 if core is stable, documentation corrections, feature
  freeze, repeated demo rehearsal.

The schedule assumes tasks can overlap where dependencies allow. If work is strictly sequential,
P1 and P2 tasks must be dropped before any P0 acceptance criterion.

## 17. First implementation slice

Begin with these tasks only:

1. B01 — Standalone workspace.
2. B02 — Linux environment.
3. C01 — Domain state machines.
4. C02 — SQLite migrations.
5. C04 — Event journal.
6. W01 — Boundary protocol.
7. W02 — Deterministic agent.
8. W04 — Direct supervisor.
9. W05 — `/proc` collector.
10. M1 acceptance.

Do not begin CRIU, dashboard, live LLM, or effect-gateway work until this vertical slice is
working and tested.
