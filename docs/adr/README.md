# Epoch architecture decisions

Architecture decision records capture choices that define Epoch's safety and experimental
boundaries. An accepted record can be superseded by a later record, but it is not silently
rewritten after implementation evidence depends on it.

- [ADR-001: Keep trusted authority outside the rollback domain](001-trusted-state-outside-rollback.md)
- [ADR-002: Store metadata in SQLite and artifacts by content hash](002-sqlite-and-content-addressed-blobs.md)
- [ADR-003: Validate the runtime with a deterministic workload first](003-deterministic-workload-first.md)
- [ADR-004: Support application-level recovery independently of CRIU](004-recovery-independent-of-criu.md)
