# Final COW and isolation performance matrix

`epoch-performance-matrix` closes the two required Phase E evidence gaps without changing the
effect, capability, demo, supervisor, or existing benchmark schemas. It is both a Rust library and
a standalone evidence command. Every configured point remains in JSON, CSV, and Markdown even when
the host cannot run it.

## Evidence contract

The required COW matrix is the Cartesian product of:

- allocations: 128 MiB, 512 MiB, and 1 GiB;
- child fan-out: 1, 2, 4, and 8;
- dirty pages: 0%, 1%, 10%, 50%, and 100%.

That produces 60 stable rows. `--include-optional-2gib` adds 20 optional rows, but it does not
override either memory guard. The Rust planner estimates the simultaneous parent, sequential
full-copy control, child-private dirty pages, and bounded process overhead before launching. The
Python helper independently repeats the preflight against live `MemAvailable`. Unsafe rows are
`skipped` with byte budgets, never omitted or attempted optimistically.

Successful raw COW samples contain allocation time, fork pause, total runtime, RSS, PSS, minor and
major faults, explicit full-copy bytes/time, and PSS/full-copy basis points. Point summaries retain
minimum, p50, p95, and maximum values.

The isolation matrix uses the existing `epoch-sandbox` `DirectBackend` and `LinuxBackend`. It
records the first launch as cold and later launches as warm, separating workload runtime from
launch overhead and recording child CPU time, peak RSS, and a compatibility result. The Linux probe
also requires the read-only base, blocked external network, zero effective capabilities,
`no_new_privs`, and seccomp filter to be observable. Unsupported discovery never runs a direct
process in its place.

Checkpoint interaction is reported honestly for both rows. The standalone direct probe has no
cooperative checkpoint boundary, and the Linux sandbox launcher is not composed with CRIU or the
composite-checkpoint coordinator, so both are currently structured `unsupported` rather than an
inferred success.

## macOS gate

Build and run from a clean committed revision:

```bash
cargo build --locked -p epoch-performance-matrix --bins
REVISION="$(git rev-parse HEAD)"
target/debug/epoch-performance-matrix \
  --output docs/evidence/performance-macos-arm64 \
  --code-revision "$REVISION"
```

macOS retains all 60 COW rows as `platform_not_linux`. If isolation fixture paths are not supplied,
both isolation rows are explicitly `fixture_unconfigured`. The checked-in
[macOS evidence](evidence/performance-macos-arm64/RESULTS.md) demonstrates this contract at the
source revision recorded in its report.

## Frozen Oracle ARM64 run

Run only after transferring the exact clean integration commit to the Ubuntu ARM64 host. Build the
performance binaries and the already-validated sandbox helper, then install only the trusted helper
as root-owned:

```bash
cargo build --locked \
  -p epoch-performance-matrix --bins \
  -p epoch-sandbox --bin epoch-sandbox-init
sudo install -d -o root -g root -m 0755 /usr/local/libexec
sudo install -o root -g root -m 0755 \
  target/debug/epoch-sandbox-init \
  /usr/local/libexec/epoch-sandbox-init
sudo install -d -m 0777 /var/tmp/epoch-performance-workspace
REVISION="$(git rev-parse HEAD)"
OUTPUT="/var/tmp/epoch-performance-${REVISION}"
sudo target/debug/epoch-performance-matrix \
  --output "$OUTPUT" \
  --code-revision "$REVISION" \
  --repetitions 3 \
  --isolation-repetitions 7 \
  --max-memory-bytes 4294967296 \
  --cow-helper "$PWD/crates/epoch-performance-matrix/helpers/cow_matrix_probe.py" \
  --python /usr/bin/python3 \
  --isolation-probe "$PWD/target/debug/epoch-performance-probe" \
  --sandbox-helper /usr/local/libexec/epoch-sandbox-init \
  --isolation-workspace /var/tmp/epoch-performance-workspace
```

The 4 GiB caller ceiling and half-of-live-available guard are both enforced. A large fan-out/dirty
row that does not fit the Oracle host therefore remains a measured preflight decision, not a crash.
Do not enable the optional 2 GiB allocation unless the normal 60-row run is complete and the live
preflight admits it.

Validate the evidence from inside the new output directory:

```bash
cd "$OUTPUT"
sha256sum --check checksums.sha256
```

Copy the complete directory without regenerating individual files. `report.json` is authoritative;
`samples.csv` is the flat audit surface and `RESULTS.md` is derived from the same in-memory report.

## Acceptance integration

The library entry points are `PerformanceRunner::new(...).run()`, `run_cow_matrix`,
`run_isolation_comparison`, and `write_artifacts`. An `epoch bench run all` adapter can call these
directly after adding the workspace dependency. Until that thin adapter lands, the stable binary
above is the required performance subcommand in the final runbook. It requires an exact 40-character
lowercase revision and refuses to overwrite an evidence directory.

