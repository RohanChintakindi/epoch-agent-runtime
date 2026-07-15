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

A completed deterministic W02 run can produce a version 1 context directly. The adapter records the
actual safe-point boundary sequence, its two model-boundary cursors, completed tool/task cursor,
seed, scenario-specific safe-point ID, and empty correlation sets. Tests capture and restore this
context and prove every cursor survives unchanged.

## Remaining integration work

This crate intentionally does not own supervisor behavior. W04 and the checkpoint coordinator still
need to:

- request and wait for a cooperative safe point;
- ingest observable content bytes into the trusted BlobStore before accepting their hashes;
- map the supervisor's durable stream cursor and pending logical tasks into `ApplicationContext`;
- persist the artifact metadata in SQLite and include it in an atomic composite epoch;
- rehydrate the agent SDK from restored context and resume only after policy/effect-frontier checks;
- stage and clean failed composite components without exposing a valid-looking epoch.

Workspace and CRIU backends remain separate K03/K04 components. Their availability must not change
an application checkpoint from unsupported or failed into a false success.
