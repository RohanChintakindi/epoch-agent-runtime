# ADR-002: Store metadata in SQLite and artifacts by content hash

- Status: Accepted
- Date: 2026-07-15

## Context

The prototype needs durable sessions, branch history, events, capabilities, effects, semantic
manifests, benchmark results, and fault injections. Checkpoint images and captured payloads can be
large, while most metadata is relational and transaction-sensitive. The first implementation must
be inspectable, reproducible, and operable on one machine without a service dependency.

## Decision

Epoch stores trusted relational metadata in a local SQLite database configured with foreign keys,
WAL journaling, FULL synchronous mode, transactional checksummed migrations, and a busy timeout.
Large or repeated byte payloads live in a filesystem content-addressed store keyed by lowercase
SHA-256. SQLite stores their hash, length, media type, ownership, and references.

Blob publication writes a temporary file, flushes it, atomically renames it to the sharded hash
path, and verifies data against its address when read. Metadata may reference a blob only after the
blob has been published successfully.

## Alternatives considered

- Put every artifact in SQLite BLOB columns. This simplifies transactions but makes large
  checkpoint IO, deduplication, and artifact inspection less convenient.
- Store all metadata as JSON files. This is easy initially but makes concurrency, referential
  integrity, monotonic sequence allocation, and migrations fragile.
- Introduce PostgreSQL and object storage immediately. Those are reasonable production options
  but add deployment and network variables that do not answer the prototype's architecture
  questions.

## Consequences

- The prototype is self-contained and easy to copy or inspect.
- Relational invariants and effect journaling can commit atomically.
- Identical artifacts are deduplicated and corruption is detectable.
- Database rows and blob publication cannot share one filesystem transaction; ordering and orphan
  cleanup must be explicit.
- SQLite write concurrency is bounded, so benchmarks must identify when a server database would be
  warranted rather than assuming unlimited scale.
