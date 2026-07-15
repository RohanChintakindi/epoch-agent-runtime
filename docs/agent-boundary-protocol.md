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
    "input_hash": "sha256:..."
  }
}
```

`sequence` is assigned by the emitting agent and gives its boundary messages a stable order. The
supervisor still assigns the durable event sequence from the runtime specification when it ingests
the record. Inputs and outputs are represented by hashes so large or sensitive content can live in
the content-addressed blob store instead of the event stream.

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
and records larger than 1 MiB including the terminator are rejected with distinct typed errors.

The 1 MiB boundary is a defensive framing limit, not permission to embed large model or tool
content. Normal messages should carry content hashes and small metadata.
