# Agent boundary protocol

Epoch's deterministic agents write one JSON object per line to the trusted supervisor. Version 1
uses this envelope:

```json
{
  "protocol_version": 1,
  "sequence": 7,
  "type": "tool.call",
  "payload": {
    "call_id": "call-1",
    "tool": "write_file",
    "input_hash": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
  }
}
```

`sequence` is assigned by the emitting agent and gives its boundary messages a stable order. The
supervisor still assigns the durable event sequence from the runtime specification when it ingests
the record. Inputs and outputs are represented by hashes so large or sensitive content can live in
the content-addressed blob store instead of the event stream.

Every hash is a bare 64-character lowercase SHA-256 digest represented by `epoch_blob::BlobHash`.
Sequence and context-revision values cannot exceed `i64::MAX`, matching SQLite's signed integer
domain. Identifiers are limited to 255 UTF-8 bytes; model and tool names are limited to 128 bytes.

## Version 1 messages

| Type | Required payload fields |
| --- | --- |
| `agent.start` | `agent_id`, `session_id`, `branch_id` |
| `context.update` | `revision`, `context_hash` |
| `model.request` | `request_id`, `model`, `input_hash` |
| `model.response` | `request_id`, `output_hash` |
| `tool.call` | `call_id`, `tool`, `input_hash` |
| `tool.result` | `call_id`, `outcome`, `output_hash` (nullable) |
| `safe_point` | `safe_point_id`, `context_hash` |
| `agent.completion` | `outcome`, `output_hash` (nullable) |

Tool outcomes are `succeeded`, `failed`, or `denied`. Completion outcomes are `succeeded`,
`failed`, or `cancelled`. Identifiers, model/tool names, and non-null hashes cannot be empty.

## Compatibility rules

- A version 1 reader accepts unknown fields on a known envelope and inside a known message payload.
  It retains those fields and emits them again if the typed message is re-encoded. This lets a
  producer add optional metadata without breaking an older supervisor.
- A reader rejects an unknown message `type` even when `protocol_version` is 1. Silently ignoring a
  boundary event could make a replay or checkpoint history unsound.
- A reader rejects unknown enum values and missing or invalid required fields with a typed protocol
  error.
- A change that removes a field, changes the meaning or type of a field, or introduces a required
  message that old supervisors cannot safely ignore requires a new `protocol_version`.
- This implementation accepts version 1 only. It returns `UnsupportedVersion` before dispatching a
  message from another version.

## Framing and limits

The decoder consumes exactly one record. A record may have no terminator, `LF`, or `CRLF`; an
encoder always emits one trailing `LF`. Empty records, multiple physical records, malformed JSON,
bare carriage returns, duplicate object keys at any nesting depth, and records larger than 1 MiB
including the terminator are rejected with distinct typed errors.

The 1 MiB boundary is a defensive framing limit, not permission to embed large model or tool
content. Normal messages should carry content hashes and small metadata.

## Trusted blob ownership

An agent-provided hash is an untrusted claim, not evidence that content exists. The trusted
supervisor owns blob ingestion: it receives or captures the referenced bytes through a separate
bounded channel, writes them with `BlobStore::put`, and performs an integrity-checked lookup before
acknowledging or persisting the boundary record. `Envelope::referenced_hashes` exposes every claim,
and `validate_referenced_blobs` rejects both missing content and content that fails verification.

The agent must never receive write authority to the trusted blob-store root. A raw deterministic
agent trace is therefore useful as a producer fixture, but it is not directly eligible for durable
event persistence until the supervisor has ingested and verified each referenced blob.
