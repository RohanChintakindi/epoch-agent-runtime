# Oracle ARM64 evidence index

The canonical Week 4 native run is
[`bench-da1681b5-799e-40ac-afdb-dfa37e94328c`](bench-da1681b5-799e-40ac-afdb-dfa37e94328c/RESULTS.md).
It measured clean source revision `463e130b628bd641cdf4be6dd516f4ef0a4fd75f` with the frozen
command in [`docs/benchmarking.md`](../../docs/benchmarking.md).

Environment: Linux `6.17.0-1018-oracle`, ARM64 Neoverse-N1, 2 logical CPUs, 12,506,804,224
bytes of memory, Rust 1.97.0.

Results:

- Application/workspace checkpoint: 20/20 retained samples succeeded across separate trace-off
  and trace-on reports; zero correctness validation failures; capture p95 956,685,457 ns; restore
  p95 173,348,675 ns.
- COW: 5/5 samples succeeded; PSS/full-copy ratio 5,419 basis points; 16,439 minor faults and zero
  major faults per sample; COW elapsed p95 411,299,072 ns.
- Compatibility: all 13 configured rows retained—3 supported scaling rows, 8 explicit
  unsupported rows, and 2 expected failed-validation rows.
- Faults: 4 actual injection rows contained successfully and 3 unavailable integration stages
  retained as symbolic unsupported. No external exactly-once claim was made.

Artifact SHA-256 values:

```text
0804226b5c2aa21159e374e2d3e2e442e3a858103b4fe76e822b873ed0ae1228  report.json
f2cf3c28e7353cd5805cce8f5828dc94d9a64c634a8adbedc1f52f834a1da2aa  samples.csv
4fb09cae581e9e53942ab525bdc3dbb47a258613d75bbf8bbdc429420482c56e  RESULTS.md
```

The JSON is authoritative. The index and generated Markdown summarize it without replacing raw
samples or unsupported/failed rows.
