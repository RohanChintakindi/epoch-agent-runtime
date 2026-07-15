# Logical fork and replay boundary

Epoch can create a logical child branch from a committed application checkpoint:

```console
epoch fork EPOCH_ID --name experiment-a
epoch branch inspect BRANCH_ID
```

Fork creation first validates the committed epoch record, application component metadata,
content-addressed bytes, canonical context encoding, and referenced observable blobs. It then
commits one branch row in an immediate SQLite transaction. The row binds the child to the source
session, parent branch, source epoch, application component hash, and application boundary
sequence. Names use a canonical lowercase ASCII form and are unique within a session; a collision
fails instead of silently adding a suffix.

The lineage columns are immutable. Fork creation does not update the parent branch, checkpoint,
component, event counter, or effect history. A child's event counter starts at zero and future
state transitions are independent of its parent. Inspection reopens and revalidates the source
component, so a corrupt or uncommitted source never appears as a usable fork.

## Replay contract

The source event history contains validated protocol records for model and tool results. Fork
inspection returns their correlation IDs, protocol positions, outcomes, and recorded hash claims
through the application resume cursor. These are useful evidence, but they are not result bytes:
the supervisor intentionally never promoted agent-supplied hashes into verified blob references.

Application context schema v1 also has no agent resume adapter. The fork response therefore marks
continuation as `unsupported` with code `agent_resume_adapter_unavailable`. It does not pretend to
have executed the agent or replayed an external action. A later replay backend must ingest and
verify result bytes, bind them to the cursor, and implement the agent-specific continuation
protocol before changing this boundary to supported.

## Trusted state outside rollback

Effect intents and attempts remain in trusted SQLite state and migration v3 rejects their deletion.
Fork and application restore do not copy, rewind, or erase them. The source epoch's numerical
effect frontier is reported, but fork inheritance is explicitly `unsupported` with code
`effect_frontier_not_integrated` until effect reconciliation is implemented.

Branch promotion is also explicitly unsupported. Promotion requires a canonical-branch pointer
and compare-and-swap revision check; setting a branch state alone would permit stale concurrent
promotion. Workspace layers, process memory, CRIU, isolation, capabilities, and authority
delegation are not part of this logical application-only fork.
