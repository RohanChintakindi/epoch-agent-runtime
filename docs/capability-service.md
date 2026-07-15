# Epoch capability service

`epoch-capabilities` is the trusted A01 prototype. It keeps current authority in the host-side
SQLite database, outside rollbackable agent checkpoints. The sandbox receives only an opaque
bearer handle; the authoritative database stores its SHA-256 digest and never the handle bytes.

## Authorization model

A capability is bound exactly to:

- session and branch;
- trusted subject identity;
- action and resource;
- current branch policy revision;
- optional expiration, use count, and budget-unit limits.

The service denies by default. Each use opens an immediate SQLite transaction, resolves the handle
digest, checks every binding against current state, checks the branch's current policy revision,
checks the capability and its full delegation ancestry, and decrements every ancestor counter.
Only then does it append an authorization record and an allow audit record in the same transaction.
A denial appends its audit record in the transaction that observed the denial.

This ancestor accounting matters: minting several attenuated children cannot multiply the root's
remaining use count or budget. Revoking, expiring, or consuming an ancestor also invalidates every
descendant. A restored checkpoint may still contain an old handle, but current trusted state wins.

Handles contain 256 bits from the operating-system CSPRNG and use the versioned
`ecap_v1_<lowercase-hex>` representation. `Debug` is redacted and there is intentionally no
`Display` implementation. `CapabilityHandle::expose` should only be used to deliver a handle over
a trusted sandbox boundary.

## Effect-gateway adapter

`CapabilityAuthorizer` implements `epoch_effects::Authorizer`. It binds one handle to a trusted
subject and fixed budget charge, translates the gateway's session, branch, operation ID, exact
action/resource, input hash, and policy revision into a capability use, and fails closed on every
error. A committed effect replay is still served by `epoch-effects` without consuming authority
again.

The authorizer runs inside the effect gateway's immediate SQLite transaction. An allow carries the
validated capability ID, and counter consumption, capability audit, effect intent, blob metadata,
and initial effect history commit or roll back together. Provider dispatch begins only after that
transaction commits. A failed effect-intent insert therefore cannot burn a one-use capability.

The authoritative capability audit is `capability_decisions`. A later event-journal projection may
copy decisions for the UI, but correctness must not depend on a cross-table event written after the
decision. If SQLite itself cannot commit, the adapter returns deny; no durable audit can be promised
during that storage failure.

## Migration integration

Migration 4, `trusted_capability_authority`, follows the effect gateway's migration 3. It adds:

- `remaining_budget_units` to the existing capability summary;
- monotonic per-branch policy revisions;
- immutable delegation ancestry;
- append-only successful authorization records;
- append-only allow/deny decision audit records;
- database triggers preventing scope mutation, counter increases, deletion, and reactivation.

Migration 6 adds the capability frontier recorded by each composite checkpoint. Restore reads the
frontier for comparison but never uses it to overwrite current authority.

## Explicit limitations

- This is a single-host SQLite prototype, not a replicated or multi-tenant authority service.
- Policy revision changes are trusted control-plane operations; operator authentication/RBAC is not
  implemented.
- Supported scope is exact action/resource matching plus expiration/use/budget constraints. There
  are no wildcards, natural-language rules, approvals, DLP, PII/NER, or external policy service.
- A stolen handle used through the same trusted subject adapter retains its remaining narrow
  authority. Process isolation and handle-delivery protection remain required.
- Capability metadata and audit fields are not encrypted and can contain sensitive identifiers.
  Raw effect input is represented only by its digest, and PII redaction is not implemented.
- Expiration relies on the trusted host wall clock; distributed clock semantics are out of scope.
- `Store::connection` remains part of the prototype TCB. Database triggers defend monotonicity and
  append-only history, but file permissions and trusted-component review are still required.
- The CLI supports strict-JSON grant, non-secret inspect/revoke-by-ID, and effect history listing.
