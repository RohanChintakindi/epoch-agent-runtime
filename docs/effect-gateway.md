# Effect gateway integration contract

`epoch-effects` is the trusted, single-machine A02/A03 prototype. It coordinates effect intent
and attempt records in Epoch's existing SQLite database and stores canonical inputs and committed
results through the existing content-addressed `BlobStore`. It does not create a second authority
database.

## Execution contract

1. The caller creates a `CanonicalIntent` from session, branch, replay key, action, resource, raw
   structured arguments, and current policy revision.
2. Epoch derives the operation ID from session, branch, and replay key. It canonicalizes the
   structured action and computes the input hash from those trusted raw bytes.
3. If that operation is already committed with the same input, Epoch verifies and returns the
   exact recorded result without authorization or downstream dispatch.
4. A new operation passes through the explicitly supplied `Authorizer`. There is no implicit
   allow implementation; `DenyAllAuthorizer` is the safe composition default.
5. Epoch stores the canonical input through `BlobStore`, inserts its metadata, and durably appends
   requested/prepared transitions.
6. Epoch durably records the dispatched transition and started attempt before calling the
   `EffectDispatcher` with the stable operation ID.
7. A successful raw response enters `BlobStore` through `put`; the gateway never accepts an
   unverified provider-supplied blob hash. The committed transition, attempt outcome, result hash,
   and mutable summaries commit in one SQLite transaction.

The downstream adapter must use the operation ID as an idempotency key. Epoch claims
effectively-once behavior only for adapters whose downstream service enforces that contract. It
does not claim universal exactly-once delivery.

Committed replay bypasses a new authority check because the external action has already happened;
returning its recorded result grants no new effect. Uncommitted existing operations fail closed.
The caller must not manufacture a new replay key to bypass an unresolved replay position.

## Crash boundaries

| Fault point | Durable state | Classification | Automatic retry |
|---|---|---|---|
| `AfterPrepared` | prepared, no attempt | known not sent | no |
| `AfterDispatchedBeforeInvoke` | unknown | conservatively unknown | no |
| `AfterInvokeBeforeCommit` | unknown | unknown downstream outcome | no |

The second boundary is deliberately conservative. The injector knows the fixture call did not run,
but a process reopening only the durable `dispatched` boundary cannot prove that fact. Unknown
effects require reconciliation or operator resolution before retry; those workflows are not part
of this lane.

## Credential boundary

Provider credentials belong inside the trusted dispatcher implementation and are absent from
`AuthorizationRequest`, `DispatchRequest`, transition details, and error categories. The crate has
no logging calls. Canonical intent validation rejects common credential-bearing fields, alternate
API-key headers, credential URL user-info, and common inline credential markers before any blob or
database write.

This validation is a defense-in-depth boundary, not a general secret scanner or DLP system. A
production adapter remains responsible for never placing its credential in action arguments,
results, downstream references, or errors. Dispatcher failures are represented by a bounded enum,
so raw provider error text cannot enter trusted history.

## Schema mapping and limitations

Migration 3 adds database-enforced append-only `effect_transition_history` and
`effect_attempt_history` tables. Update and delete triggers protect both even when a caller obtains
the trusted raw SQLite connection.

The original migration's mutable summary vocabulary remains unchanged for compatibility:

- public/history `committed` maps to `effect_intents.state = 'succeeded'`;
- public/history `requested` is an immutable transition while the summary row begins at
  `prepared`;
- `effect_attempts` remains a mutable one-row summary per attempt; `effect_attempt_history` is the
  append-only audit source.

These naming differences should be removed in a future table-rebuild migration, not by changing
the checksum of migration 1. Other current integration limits are explicit:

- `Authorizer` is a seam, not A01 capability enforcement. Production wiring must validate current
  branch-bound authority and must not use a test allow-all authorizer.
- Capability IDs remain null until A01 supplies validated authority; this crate does not pretend a
  capability was granted.
- Unknown-outcome reconciliation, approval, compensation, and branch suspension are not yet
  implemented.
- Concurrent callers are suppressed, but a caller racing the active dispatch receives an
  unresolved-state error rather than waiting.
- The event journal is a derived observability projection, not the effect source of truth. A later
  integration may emit effect events after commits, but correctness must depend on the effect
  histories because the current `EventJournal` cannot append in the same SQLite transaction.
- CLI and supervisor wiring are intentionally outside this isolated library lane.

The `DeterministicLocalDispatcher` is the local idempotent fixture for tests and demonstrations.
No commits were cherry-picked from the separate HTTP mock-effects branch.
