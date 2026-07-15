# CRIU compatibility evidence

## Environment

| Field | Value |
| --- | --- |
| Code revision | 11739a8e3f2da9673d642ac718cee8bbb39dc229 |
| Operating system | linux |
| Architecture | aarch64 |
| Kernel release | 6.17.0-1018-oracle |
| CRIU version | Version: 4.2 |
| CRIU basic check | supported |
| CRIU extended check | warnings or unsupported |

## Compatibility matrix

| Scenario | Memory bytes | Processes | Status | Dump ms | Restore ms | Image bytes | Verification | Diagnostic |
| --- | ---: | ---: | --- | ---: | ---: | ---: | --- | --- |
| sleeping_process | 4194304 | 1 | supported | 31 | 10 | 4325440 | yes | dump, restore, behavior verification, and cleanup succeeded |
| sleeping_process | 67108864 | 1 | supported | 103 | 41 | 67236190 | yes | dump, restore, behavior verification, and cleanup succeeded |
| open_regular_file | 4194304 | 1 | supported | 21 | 30 | 4321446 | yes | dump, restore, behavior verification, and cleanup succeeded |
| open_regular_file | 67108864 | 1 | supported | 53 | 41 | 67240393 | yes | dump, restore, behavior verification, and cleanup succeeded |
| process_tree | 4194304 | 2 | supported | 20 | 20 | 4451711 | yes | dump, restore, behavior verification, and cleanup succeeded |
| process_tree | 4194304 | 4 | supported | 20 | 20 | 4696053 | yes | dump, restore, behavior verification, and cleanup succeeded |
| process_tree | 67108864 | 2 | supported | 51 | 40 | 67362463 | yes | dump, restore, behavior verification, and cleanup succeeded |
| process_tree | 67108864 | 4 | supported | 51 | 41 | 67615003 | yes | dump, restore, behavior verification, and cleanup succeeded |
| loopback_socket | 4194304 | 1 | supported | 30 | 40 | 4325841 | yes | dump, restore, behavior verification, and cleanup succeeded |
| loopback_socket | 67108864 | 1 | supported | 50 | 70 | 67240694 | yes | dump, restore, behavior verification, and cleanup succeeded |
| external_tcp | 4194304 | 1 | unsupported | — | — | — | no | external TCP depends on remote peer state and is explicitly outside the transparent restore subset |
| workspace_mutation | 4194304 | 1 | supported | 20 | 10 | 4321359 | yes | dump, restore, behavior verification, and cleanup succeeded |
| workspace_mutation | 67108864 | 1 | supported | 40 | 40 | 67240306 | yes | dump, restore, behavior verification, and cleanup succeeded |

## Keep/narrow/kill evidence

The predeclared `narrow_or_kill` gates require 100% restore correctness for declared supported rows, checkpoint pause p95 at or below 1000 ms, and restore p95 at or below 3000 ms.

Recommendation: `keep`. Every declared in-scope row restored correctly within the preliminary latency gates.
