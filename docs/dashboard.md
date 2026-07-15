# Local runtime dashboard

The Epoch dashboard is a read-only inspection surface for an existing trusted state directory. It
is intentionally local: there is no authentication layer, and `epoch serve` refuses every bind
address that is not an IPv4 or IPv6 loopback address.

## Run it

From the workspace root, point the server at the directory that contains `state.db`:

```sh
cargo run --locked -p epoch-cli -- serve \
  --state-root /absolute/path/to/existing/.epoch \
  --bind 127.0.0.1:8080
```

Then open `http://127.0.0.1:8080/`. The default bind is `127.0.0.1:8080`, so the shorter equivalent
is:

```sh
cargo run --locked -p epoch-cli -- serve --state-root /absolute/path/to/existing/.epoch
```

For an `epoch demo` run, the state root is `<run_root>/.epoch`, where `run_root` is reported by the
demo command. The dashboard does not create a state directory or migrate a database; a missing,
corrupt, symlinked, or incompatible database is refused.

Benchmark cards are loaded from `<state-root>/benchmarks` when that directory exists. To read a
different result directory:

```sh
cargo run --locked -p epoch-cli -- serve \
  --state-root /absolute/path/to/existing/.epoch \
  --results-root /absolute/path/to/benchmark-results \
  --bind 127.0.0.1:8080
```

Do not expose the listener with port forwarding or a reverse proxy. Add a real authentication and
authorization boundary before changing the loopback restriction.

## What it shows

The interface uses only persisted runtime data and current backend discovery:

- session list, status, policy revision, and branch/epoch counts;
- immutable branch lineage and fork points;
- per-branch ordered event timelines with actor, kind, and status filters;
- checkpoint components, frontiers, and recorded application restore outcomes;
- persisted semantic diff classifications and paths;
- current capability state and append-only decision history;
- effect intents, transitions, attempts, and attempt history;
- explicit supported or unsupported backend registrations; and
- bounded benchmark result cards, or an explicit unavailable state.

The UI polls at a fixed five-second interval only while the page is visible. It has no WebSocket,
mutation action, external font, analytics script, CDN, or remote asset.

## Read-only JSON API

All endpoints accept `GET` and `HEAD` only. Mutation methods return `405` with `Allow: GET, HEAD`.

| Endpoint | Purpose |
| --- | --- |
| `/api/v1/sessions?status=&offset=&limit=` | Bounded session page |
| `/api/v1/sessions/{session_id}` | Session and branch lineage |
| `/api/v1/branches/{branch_id}/timeline?actor=&kind=&status=&offset=&limit=` | Ordered, filtered event page |
| `/api/v1/sessions/{session_id}/epochs?offset=&limit=` | Epoch components and restore outcomes |
| `/api/v1/sessions/{session_id}/diffs?offset=&limit=` | Redacted semantic diff summaries |
| `/api/v1/sessions/{session_id}/capabilities` | Current authority and decision audit |
| `/api/v1/sessions/{session_id}/effects` | Intent, transition, and attempt history |
| `/api/v1/backends` | Current host support and registered boundaries |
| `/api/v1/benchmarks` | Bounded result cards or unavailable reason |

Session and epoch pages are capped at 100 records, semantic diff pages at 50, and timeline pages at
200. Invalid, duplicated, unknown, or oversized query parameters are rejected. The largest accepted
request target is 2 KiB.

## Trust and redaction boundary

The browser never opens SQLite. The Rust read model opens `state.db` with SQLite read-only and
`query_only` flags, validates the expected schema and database integrity, uses parameterized bounded
queries, and returns narrow typed response objects.

The API deliberately excludes:

- event `payload_json` and payload blobs, including captured stderr;
- capability handles and stored handle digests;
- request, input, output, response, and blob hashes that are not needed for inspection;
- effect `error_json`, transition details, downstream references, and provider responses; and
- semantic diff before/after values. Diff section, path, classification, and counts remain visible.

JSON uses standard serialization plus HTML-sensitive Unicode escaping. The static UI creates DOM
nodes with `textContent`; it does not render trusted-state strings as HTML. Every response includes a
deny-by-default Content Security Policy, `nosniff`, frame denial, same-origin opener/resource policy,
no-referrer, disabled browser capabilities, and `Cache-Control: no-store`.

This is a local inspection tool, not an authentication boundary and not an agent-facing control
plane.
