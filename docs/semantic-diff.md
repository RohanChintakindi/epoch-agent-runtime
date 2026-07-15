# Application semantic diff

`epoch-diff` compares two trusted application checkpoint components and emits semantic diff schema
version 1 as deterministic JSON. It does not accept a raw `ApplicationContext`: callers supply the
checkpoint records and the application checkpoint backend that owns their content-addressed blobs.
The backend restores both records before comparison, enforcing schema, shape, nested reference,
content-hash, byte-length, canonical-encoding, and metadata validation.

The integration boundary is:

```rust
diff_application_checkpoints(&backend, &before_record, &after_record)
    -> Result<ApplicationSemanticDiff, DiffError>
```

Changes use JSON Pointer paths, an `added`, `removed`, or `changed` classification, before/after
JSON values, and a typed semantic section. Keyed entities are compared by their stable IDs and all
changes are sorted by path. A pure reorder of messages or the pending-task queue is reported because
order can alter model context or the next task selected. Pending operation IDs are treated as
correlation sets; their serialized order is not semantic in application-context schema 1.

The application-context component represents resume cursors, model/tool configuration, observable
messages, pending tasks and operation IDs, task payload references, message content references, and
the user-visible summary reference. It does **not** represent a workspace file manifest, the current
branch capability set, or the trusted effect frontier, so its component-level diff explicitly
lists those sections as unsupported. The supervisor's composite epoch diff adds validated workspace
manifests plus recorded/current capability and effect frontiers. It does not infer either security
domain from task text, summaries, or hidden reasoning state.

Unknown application-context fields are rejected by the checkpoint decoder. A future schema version
returns `unsupported_schema`, while corrupt, missing, non-canonical, or metadata-mismatched inputs
return `invalid_checkpoint` with a stable code and the side that failed.
