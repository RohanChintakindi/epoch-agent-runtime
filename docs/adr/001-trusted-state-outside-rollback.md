# ADR-001: Keep trusted authority outside the rollback domain

- Status: Accepted
- Date: 2026-07-15

## Context

An agent checkpoint can contain process memory, local files, execution context, and cached
credentials. Restoring that checkpoint rewinds those values. Capability policy, approval
decisions, and records of external effects must not rewind with it: a restored agent must not
regain revoked authority or forget an email, payment, or other effect that already occurred.

## Decision

Epoch separates state into two trust domains:

1. Rollbackable execution state contains the sandboxed process tree, agent context, and workspace.
2. Monotonic trusted state contains current capability policy, approvals, effect intents and
   attempts, audit events, and committed checkpoint metadata.

The trusted state lives in the supervisor's control plane outside the sandbox snapshot. A restore
always reconciles the restored execution state against current trusted state before execution
resumes. A checkpoint carries a policy revision and effect frontier for comparison; it is not the
authority for either value.

## Alternatives considered

- Snapshot the entire control plane with the agent. This makes restoration simple but can revive
  expired capabilities and duplicate external effects.
- Let each tool track its own history. This distributes correctness across integrations and leaves
  no single replay or audit boundary.
- Treat all effects as reversible. Many real effects are irreversible or only compensatable, so
  this is not a general safety model.

## Consequences

- Restores require an explicit reconciliation phase and may suspend for operator review.
- Capability handles must be revalidated against current policy after every restore.
- External effects require stable operation IDs and durable intent/attempt records.
- The control plane becomes trusted computing base and needs stronger durability and access
  controls than the sandbox.
- A demo can clearly show that workspace state rewinds while revoked access and effect history do
  not.
