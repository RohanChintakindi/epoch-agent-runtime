# ADR-004: Support application-level recovery independently of CRIU

- Status: Accepted
- Date: 2026-07-15

## Context

CRIU can capture Linux process state with high fidelity, but support depends on kernel features,
privileges, process behavior, open resources, architecture, and deployment environment. It does not
run on macOS and may fail for sockets, devices, namespaces, or unsupported kernel state. A runtime
whose only recovery path is CRIU cannot provide a portable or predictable product contract.

## Decision

Epoch treats process-image checkpointing as one optional backend. The stable recovery contract is
application/workspace recovery built from:

- content-addressed workspace snapshots or filesystem deltas;
- serialized agent context at explicit safe points;
- the durable boundary event journal;
- current capability and effect reconciliation from the trusted control plane.

A backend advertises its capabilities explicitly. CRIU may add transparent in-flight process
restoration on supported Linux hosts, but failure or absence falls back to restarting the workload
from a committed application-level epoch when the scenario permits it.

## Alternatives considered

- Require CRIU for every checkpoint. This maximizes process fidelity but makes macOS development,
  restricted containers, and unsupported workloads dead ends.
- Use only application serialization. This is portable and debuggable but cannot preserve arbitrary
  stacks, child processes, memory, or open descriptors transparently.
- Use a full VM snapshot as the sole primitive. It provides a wider boundary but costs more startup,
  storage, and operational complexity and still requires external-effect reconciliation.

## Consequences

- Epoch can demonstrate recovery before privileged Linux checkpointing is complete.
- Checkpoints must declare their captured components and backend; partial snapshots never masquerade
  as complete epochs.
- Workloads need safe points and versioned context serialization for the portable path.
- CRIU and later VM backends can be benchmarked as architecture options rather than embedded as
  unquestioned assumptions.
- Some executions will be restartable only from safe points, not at arbitrary instructions.
