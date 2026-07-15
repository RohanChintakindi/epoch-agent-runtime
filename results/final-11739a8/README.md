# Final candidate evidence

This directory preserves the final acceptance evidence measured from clean source revision
`11739a8e3f2da9673d642ac718cee8bbb39dc229`. The evidence files embed that revision and report
`code_dirty: false`. They are committed by a later evidence-only commit, so repository `HEAD` is
expected to be newer than the revision under test.

Verify each platform bundle before interpreting it:

```bash
(cd macos-arm64 && shasum -a 256 -c SHA256SUMS)
(cd oracle-arm64 && sha256sum -c SHA256SUMS)
```

## Acceptance summary

| Gate | macOS ARM64 | Oracle Linux ARM64 |
|---|---|---|
| Workspace tests, all targets/features, strict Clippy, formatting | Passed | Passed |
| Deterministic demo | Three rehearsals, 13/13 phases each | 13/13 phases |
| Workspace restore | Restored bytes matched in every rehearsal | Restored bytes matched |
| Final COW matrix | 60/60 structured unsupported because `/proc` COW evidence is Linux-only; 0 failed | 54 supported, 6 skipped by the 4 GiB live-memory ceiling, 0 failed |
| Execution comparison | Direct supported; Linux explicitly unsupported on macOS | Direct and Linux supported, seven samples each |
| Effect safety campaign | 100 replays produced one durable intent and one dispatch; unknown suspended; revoked/rolled-back authority stayed denied | Same campaign passed |
| Privileged isolation | Not applicable | Native namespace/cgroup/seccomp suite passed 3/3 |
| CRIU | Not applicable | 12/12 in-scope rows restored and verified; external TCP remained the one explicit unsupported row |

The Oracle benchmark completed as `completed_with_unsupported` because two composition claims stay
explicitly unsupported: the standalone probe is not a cooperative application checkpoint, and the
Linux sandbox is not yet composed with the CRIU/composite-checkpoint coordinator. No COW row,
isolation launch, or actual fault/effect campaign failed.

## Artifact map

- `*/quality/*-quality-gate.log`: complete test, Clippy, format, diff, and clean-tree gates.
- `*/demo/*.json`: revision-pinned 13-phase demo reports.
- `*/benchmark/*/report.json`: version-2 machine-readable benchmark evidence.
- `*/benchmark/*/samples.csv` and `RESULTS.md`: stable CSV and human views regenerated from the
  same benchmark report.
- `oracle-arm64/isolation/isolation-native.log`: privileged Linux isolation evidence.
- `oracle-arm64/criu/compatibility/`: CRIU report, matrix, and per-row raw logs.
- `oracle-arm64/criu/*-native.log`: privileged CRIU integration and command logs.

## Remaining boundaries

This candidate does not claim autonomous post-fork continuation, live-provider delivery
reconciliation, a CLI supervisor adapter for the Linux backend, or a production process-memory
checkpoint backend. CRIU remains a narrow compatibility experiment, not proof that arbitrary
processes can be restored safely.
