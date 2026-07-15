# Final performance matrix

Revision `26b389efa9ac0dc42095b63833d3039a3b1d6a67` on macos aarch64 / kernel `25.5.0`. Safety budget: 0 bytes.

## COW matrix

Rows: 60 total, 0 supported, 0 skipped, 60 unsupported, 0 failed.

| Allocation | Fan-out | Dirty bps | Status | Runtime p50 ns | Fork pause p95 ns | PSS/full-copy p50 bps | Diagnostic |
|---:|---:|---:|---|---:|---:|---:|---|
| 134217728 | 1 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 1 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 1 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 1 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 1 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 2 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 2 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 2 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 2 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 2 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 4 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 4 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 4 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 4 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 4 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 8 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 8 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 8 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 8 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 134217728 | 8 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 1 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 1 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 1 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 1 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 1 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 2 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 2 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 2 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 2 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 2 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 4 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 4 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 4 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 4 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 4 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 8 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 8 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 8 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 8 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 536870912 | 8 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 1 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 1 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 1 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 1 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 1 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 2 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 2 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 2 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 2 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 2 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 4 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 4 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 4 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 4 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 4 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 8 | 0 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 8 | 100 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 8 | 1000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 8 | 5000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |
| 1073741824 | 8 | 10000 | unsupported | 0 | 0 | 0 | platform_not_linux: fork/COW PSS evidence requires Linux /proc |

## Isolation comparison

| Backend | Status | Cold total ns | Warm total p50 ns | Warm launch overhead p50 ns | Warm CPU p50 ns | Peak RSS bytes | Checkpoint interaction |
|---|---|---:|---:|---:|---:|---:|---|
| Direct | unsupported | 0 | 0 | 0 | 0 | 0 | unsupported: direct performance fixture was unavailable; checkpoint interaction was not measured |
| Linux | unsupported | 0 | 0 | 0 | 0 | 0 | unsupported: Linux performance fixture was unavailable; checkpoint interaction was not measured |
