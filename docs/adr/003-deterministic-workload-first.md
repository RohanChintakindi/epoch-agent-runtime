# ADR-003: Validate the runtime with a deterministic workload first

- Status: Accepted
- Date: 2026-07-15

## Context

Live language models introduce provider latency, nondeterministic output, rate limits, credentials,
and changing model behavior. Those variables can hide whether a mismatch came from checkpointing,
event capture, replay, or the model. Epoch first needs evidence that the runtime itself preserves
and branches execution correctly.

## Decision

The first supervised agent is a deterministic workload driven by an explicit seed and recorded
model responses. It exercises the same boundary protocol and representative behavior as a live
agent: context updates, model requests and responses, tool calls, file mutation, child processes,
memory allocation, safe points, network calls to local mocks, and configurable crashes.

Repeated runs with the same seed must produce equal normalized boundary histories and semantic
state hashes. Live model integration begins only after this invariant is demonstrated.

## Alternatives considered

- Start with a production agent framework and real model. This looks realistic but makes failures
  harder to reproduce and requires secrets before runtime correctness is known.
- Use only unit tests and fixtures. They are necessary but do not exercise process supervision,
  IO framing, persistence, or crash boundaries end to end.
- Record one live run and replay it forever. A recorded trace helps, but selectable deterministic
  scenarios provide broader, controlled fault coverage.

## Consequences

- Week 1 tests need no provider credentials or network access.
- Checkpoint, replay, and fault results are reproducible in CI and interviews.
- The workload is intentionally not evidence of live-model quality or provider integration.
- Boundary design stays provider-neutral, making a later live agent an adapter rather than a
  runtime rewrite.
