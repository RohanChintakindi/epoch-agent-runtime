# CRIU compatibility evidence

## Environment

| Field | Value |
| --- | --- |
| Operating system | linux |
| Architecture | aarch64 |
| Kernel release | 6.17.0-1018-oracle |
| CRIU version | Version: 4.2 |
| CRIU basic check | supported |
| CRIU extended check | warnings or unsupported |

## Compatibility matrix

| Scenario | Memory bytes | Processes | Status | Dump ms | Restore ms | Image bytes | Verification | Diagnostic |
| --- | ---: | ---: | --- | ---: | ---: | ---: | --- | --- |
| sleeping_process | 4194304 | 1 | supported | 33 | 10 | 4321320 | yes | dump, restore, behavior verification, and cleanup succeeded |
| sleeping_process | 67108864 | 1 | supported | 50 | 32 | 67240263 | yes | dump, restore, behavior verification, and cleanup succeeded |
| open_regular_file | 4194304 | 1 | supported | 20 | 10 | 4321423 | yes | dump, restore, behavior verification, and cleanup succeeded |
| open_regular_file | 67108864 | 1 | supported | 50 | 40 | 67240368 | yes | dump, restore, behavior verification, and cleanup succeeded |
| process_tree | 4194304 | 2 | supported | 20 | 20 | 4451683 | yes | dump, restore, behavior verification, and cleanup succeeded |
| process_tree | 4194304 | 4 | supported | 32 | 43 | 4704226 | yes | dump, restore, behavior verification, and cleanup succeeded |
| process_tree | 67108864 | 2 | supported | 88 | 124 | 67366537 | yes | dump, restore, behavior verification, and cleanup succeeded |
| process_tree | 67108864 | 4 | supported | 248 | 65 | 67614975 | yes | dump, restore, behavior verification, and cleanup succeeded |
| loopback_socket | 4194304 | 1 | supported | 30 | 40 | 4321725 | yes | dump, restore, behavior verification, and cleanup succeeded |
| loopback_socket | 67108864 | 1 | supported | 91 | 68 | 67236576 | yes | dump, restore, behavior verification, and cleanup succeeded |
| external_tcp | 4194304 | 1 | unsupported | — | — | — | no | external TCP depends on remote peer state and is explicitly outside the transparent restore subset |
| workspace_mutation | 4194304 | 1 | supported | 20 | 22 | 4325431 | yes | dump, restore, behavior verification, and cleanup succeeded |
| workspace_mutation | 67108864 | 1 | supported | 51 | 40 | 67240282 | yes | dump, restore, behavior verification, and cleanup succeeded |

## Keep/narrow/kill evidence

The predeclared `narrow_or_kill` gates require 100% restore correctness for declared supported rows, checkpoint pause p95 at or below 1000 ms, and restore p95 at or below 3000 ms.

Recommendation: `keep`. Every declared in-scope row restored correctly within the preliminary latency gates.
