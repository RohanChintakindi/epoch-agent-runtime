# Epoch benchmark report

Run `bench-a6c32a7b-e437-4a70-8f8b-980d60e28a83` used revision `11739a8e3f2da9673d642ac718cee8bbb39dc229` on linux aarch64 (Neoverse-N1, 2 CPUs).

## Predeclared thresholds

- checkpoint capture p95: at most 2000000000 ns
- checkpoint restore p95: at most 2000000000 ns
- checkpoint validation failures: at most 0
- COW PSS/full-copy ratio: at most 7500 basis points

## Keep / narrow / kill

| Decision | Mechanism | Evidence |
|---|---|---|
| Keep | cooperative application + full-copy workspace checkpoint | successful_samples=20; validation_failures=0; capture_p95_ns=294681184; restore_p95_ns=113564965 |
| Narrow | fork COW process-memory optimization | matrix_points=5; succeeded=5; unsupported=0; failed=0; maximum_pss_to_full_copy_basis_points=38147; scope remains process-memory compatibility only |
| Keep | durable effect gateway replay and fail-closed authority | effect_replay_100_runs=succeeded; effect_unknown_suspends_branch=succeeded; capability_revocation_resurrection_blocked=succeeded; capability_policy_rollback_blocked=succeeded |
| Kill | transparent external exactly-once through rollback | the integrated gateway proves durable local duplicate suppression, not a live provider's commit semantics; live provider reconciliation remains an explicit unsupported matrix row |

## Final performance matrix

COW rows: 60 total, 54 supported, 6 skipped, 0 unsupported, 0 failed. Isolation comparison: `supported` (direct `supported`, Linux `supported`).

Unsupported and failed rows remain in the JSON and CSV artifacts; no external exactly-once guarantee is inferred.
