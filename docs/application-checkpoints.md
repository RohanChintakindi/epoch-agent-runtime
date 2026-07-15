# Application checkpoints

Epoch's application backend captures observable, cooperatively resumable agent state without Linux
process checkpointing. It is the portable checkpoint layer used when CRIU is unavailable and the
application component of a future composite epoch when process snapshots are supported.

## Version 1 context

The canonical JSON context contains:

- the safe-point ID, deterministic seed, and context revision;
- absolute boundary, message, tool, and logical-task cursors;
- the observable model identifier and a deterministically ordered tool registry;
- message IDs, roles, and content hashes;
- pending logical tasks and their optional payload hashes;
- pending model-request and tool-call correlation IDs;
- an optional hash of the user-visible summary.

Raw message/task content remains in the trusted content-addressed store. The context contains typed
`BlobHash` references only. Capture fails unless every referenced blob exists and passes integrity
verification.

There is deliberately no field for hidden reasoning, scratchpad tokens, model activations, or
chain-of-thought. A model response can be resumed only from observable messages, recorded boundary
results, tasks, and cursors. Epoch does not claim to reconstruct unexposed model state.

## Deterministic and secure storage

Version 1 serializes a fixed Rust structure with ordered fields and `BTreeMap` registries. Capture
stores those exact bytes using the hardened BlobStore and returns an artifact with a mandatory
component hash, byte length, schema version, and safe-point/revision/boundary metadata.

The BlobStore creates managed directories and files as `0700` and `0600` on Unix, rejects symlinks
and unsafe existing permissions, publishes durable no-clobber writes, pins a media type to each
hash, and bounds reads and writes. The caller still owns selection of a trusted parent directory.

Restore performs these checks before returning context:

1. The artifact and embedded schema versions are supported.
2. The component exists, is within the configured size limit, and matches its SHA-256 address.
3. The stored byte length matches the artifact.
4. JSON satisfies the closed version 1 schema and semantic bounds.
5. Every nested content reference still exists and verifies.
6. Re-serialization matches the stored bytes exactly, rejecting alternate or ambiguous encodings.
7. Safe-point, context-revision, and boundary metadata match the serialized state.

An unknown schema returns `BackendOutcome::Unsupported`. Corruption, missing content, invalid
context, I/O, and metadata mismatches return `BackendOutcome::Failed` with a structured stage and
code. Only a fully hashed artifact with mandatory metadata can be `Supported`.

## W02 adapter

A completed deterministic W02 run includes the raw version 1 context in its supervisor-captured
summary. The adapter records the actual safe-point boundary sequence, its two model-boundary
cursors, completed tool/task cursor, seed, scenario-specific safe-point ID, and empty correlation
sets. Before capture, the supervisor recomputes the normalized-state hash from the captured raw
state and binds it to the durable safe-point event, safe-point ID, seed, tool registry, and cursors.
Agent-emitted hash strings are not registered as trusted blobs.

## Recovery vertical slice

The direct supervisor now implements an application-only recovery path:

1. `epoch run --manifest ...` completes a cooperative W02 run and durably records its raw stderr
   summary and validated boundary stream.
2. `epoch checkpoint SESSION [--branch BRANCH]` extracts and validates that captured context, stores
   canonical bytes, and atomically commits an `application_context` snapshot component and epoch in
   SQLite.
3. `epoch restore EPOCH` reopens the component after any supervisor restart, verifies schema,
   hashes, length, canonical encoding, nested references, and metadata, then appends an immutable
   activation event.
4. `epoch status SESSION` resolves the latest activation event and revalidates its epoch before
   returning the current observable context.

Checkpoint, restore, and status emit stable JSON with `supported`, `unsupported`, or `failed`
outcomes. Unsupported operations exit 3; validation or persistence failures exit 125. Restore
`--mode inspect` validates without activating. `fork-on-divergence` is explicitly unsupported in
this slice.

The committed epoch owns trusted SQLite metadata for the component hash, checksum, byte length,
media type, backend, schema, safe point, revision, boundary cursor, optional label, and restore
scope. Blob publication occurs before the atomic metadata transaction; an interrupted metadata
commit can leave only an unreachable content-addressed blob, never a committed-looking epoch.

## Honest scope and remaining work

This is cooperative **application-context restore**, not process or workspace rollback. The current
W02 agent reaches its safe point immediately before successful completion, so restore reactivates a
durable context for inspection and future SDK rehydration; it does not resume an instruction pointer
or relaunch the completed agent. JSON reports therefore always state `process_restored: false` and
`workspace_restored: false`. Tests mutate the workspace after checkpoint and prove application
restore leaves that mutation untouched.

The next composite recovery layer still needs to:

- add a live pause/acknowledgement exchange for agents with work remaining;
- ingest observable content bytes into the trusted BlobStore before accepting their hashes;
- rehydrate the agent SDK from restored context and resume only after policy/effect-frontier checks;
- coordinate application, workspace, and optional process components in one composite epoch.

Workspace and CRIU backends remain separate K03/K04 components. Their availability must not change
an application checkpoint from unsupported or failed into a false success.
