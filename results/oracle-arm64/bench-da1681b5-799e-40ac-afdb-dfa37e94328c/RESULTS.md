# Epoch benchmark report

Run `bench-da1681b5-799e-40ac-afdb-dfa37e94328c` used revision `463e130b628bd641cdf4be6dd516f4ef0a4fd75f` on linux aarch64 (Neoverse-N1, 2 CPUs).

## Predeclared thresholds

- checkpoint capture p95: at most 2000000000 ns
- checkpoint restore p95: at most 2000000000 ns
- checkpoint validation failures: at most 0
- COW PSS/full-copy ratio: at most 7500 basis points

## Keep / narrow / kill

| Decision | Mechanism | Evidence |
|---|---|---|
| Keep | cooperative application + full-copy workspace checkpoint | successful_samples=20; validation_failures=0; capture_p95_ns=956685457; restore_p95_ns=173348675 |
| Narrow | fork COW process-memory optimization | 5 Linux raw samples; pss_to_full_copy_basis_points=5419; scope remains process-memory compatibility only |
| Kill | transparent external exactly-once through rollback | no external effect gateway API is present in this benchmark base; symbolic stages are reported unsupported, never exactly-once |

Unsupported and failed rows remain in the JSON and CSV artifacts; no external exactly-once guarantee is inferred.
