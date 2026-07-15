# Epoch trajectory schema v1

Epoch exports one JSON object per branch and one object per line. The schema exists for offline,
advisory experiments; it is not an authorization or effect-dispatch interface.

## Privacy boundary

The exporter reads durable runtime metadata and deliberately omits event payloads, prompts,
reasoning text, tool arguments and results, paths, URLs, blob hashes, capability handles, effect
arguments, credentials, and raw runtime identifiers. Raw session, branch, task, and candidate
identifiers are replaced with domain-separated SHA-256 pseudonyms before they leave trusted state.
Pseudonymization is not anonymization: `--task-group` must itself be a non-sensitive stable token,
because a predictable low-entropy value could be guessed from its digest.

Event kinds use a finite taxonomy. Known runtime events retain their registered category; every
unknown or dynamically named event becomes `other`. This prevents an arbitrary normalized string
from becoming a covert secret channel or being persisted in the learned vocabulary.

The feature timeline stops before the first terminal outcome event (`agent.completion`,
`process.exited`, or `supervisor.failure`) and never includes that event, anything after it, or the
terminal branch state. Labels are derived separately from trusted terminal state, so changing only
a label cannot change the model input.

## Record contract

Every record contains exactly:

- `schema_version`: integer `1`.
- `privacy_profile`: `metadata_only`.
- `trajectory_id`, `task_group_id`, `session_group_id`, and `candidate_group_id`: opaque
  64-character lowercase hexadecimal SHA-256 pseudonyms.
- `branch_depth`: nonnegative branch depth.
- `success_label`: Boolean for terminal branches, otherwise `null`.
- `value_label`: finite number in `[0, 1]` for terminal branches, otherwise `null`.
- `events`: zero to 256 pre-outcome events in stable order.
- `summary`: counters recomputed only from the exported pre-outcome events.

`success_label` and `value_label` must either both be present or both be `null`. Version 1 uses an
explicit terminal-outcome proxy: promoted is `(true, 1.0)`, completed is `(true, 0.75)`, and failed
or abandoned is `(false, 0.0)`. This proxy is useful for validating the learning machinery; it is
not a claim that production task utility has been solved. A production experiment must version and
pre-register its own outcome rubric.

Each event contains exactly:

- `position`: a contiguous integer starting at zero within the pre-outcome prefix.
- `delta_monotonic_ns`: elapsed monotonic nanoseconds since the previous exported event; zero for
  the first event.
- `actor`: `agent`, `supervisor`, `tool`, `gateway`, or `operator`.
- `kind`: one registered taxonomy value or `other`.
- `status`: `started`, `succeeded`, `failed`, `denied`, or `unknown`.
- `references_epoch` and `has_causal_parent`: structural Booleans.

The summary contains `event_count`, `duration_monotonic_ns`, and counts for each status. Readers
must reject a record if the summary does not exactly match its events.

The canonical labelled and unlabelled examples live in
`crates/epoch-trajectory/tests/fixtures/schema-v1.jsonl`. Rust and Python both load that fixture so
either side fails tests if their interpretation of version 1 drifts.

## Leakage-resistant grouping

`task_group_id` is the train/validation/test unit. Every execution of the same underlying task or
repository must be exported with the same caller-supplied `--task-group`; sibling candidates also
share `candidate_group_id`. Split manifests persist the dataset digest and exact task-group
assignment. Evaluation recomputes the deterministic split and refuses any dataset, configuration,
group, or assignment drift.

The model may score an unlabelled trajectory, but training and metrics use labelled records only.
Predictions contain only the opaque trajectory ID, bounded success probability, bounded value
score, and model source. Epoch's deterministic policy remains the sole authority for capabilities,
effects, restoration, and branch promotion.
