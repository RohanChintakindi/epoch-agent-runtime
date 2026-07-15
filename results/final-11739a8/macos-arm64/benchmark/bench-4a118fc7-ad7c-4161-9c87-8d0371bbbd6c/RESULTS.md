# Epoch benchmark report

Run `bench-4a118fc7-ad7c-4161-9c87-8d0371bbbd6c` used revision `11739a8e3f2da9673d642ac718cee8bbb39dc229` on macos aarch64 (Apple M4, 10 CPUs).

## Predeclared thresholds

- checkpoint capture p95: at most 2000000000 ns
- checkpoint restore p95: at most 2000000000 ns
- checkpoint validation failures: at most 0
- COW PSS/full-copy ratio: at most 7500 basis points

## Keep / narrow / kill

| Decision | Mechanism | Evidence |
|---|---|---|
| Keep | cooperative application + full-copy workspace checkpoint | successful_samples=20; validation_failures=0; capture_p95_ns=637264833; restore_p95_ns=143842916 |
| Narrow | fork COW process-memory optimization | matrix_points=5; succeeded=0; unsupported=5; failed=0; maximum_pss_to_full_copy_basis_points=unavailable; scope remains process-memory compatibility only |
| Keep | durable effect gateway replay and fail-closed authority | effect_replay_100_runs=succeeded; effect_unknown_suspends_branch=succeeded; capability_revocation_resurrection_blocked=succeeded; capability_policy_rollback_blocked=succeeded |
| Kill | transparent external exactly-once through rollback | the integrated gateway proves durable local duplicate suppression, not a live provider's commit semantics; live provider reconciliation remains an explicit unsupported matrix row |

## Final performance matrix

COW rows: 60 total, 0 supported, 0 skipped, 60 unsupported, 0 failed. Isolation comparison: `unsupported` (direct `supported`, Linux `unsupported`).

Unsupported and failed rows remain in the JSON and CSV artifacts; no external exactly-once guarantee is inferred.
