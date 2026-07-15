# CRIU compatibility and scaling prototype

`epoch-criu-compat` is a bounded experiment runner for deciding where transparent CRIU process
checkpointing is credible. It is not registered as Epoch's production process-checkpoint backend.
The existing application and workspace recovery paths remain independent of CRIU.

## What it measures

The version-1 runner records the operating system, architecture, kernel release, effective UID,
CRIU version, basic `criu check`, and extended `criu check --all` diagnostics. It then runs a stable
declared matrix:

- sleeping process;
- an open regular file whose offset continues advancing after restore;
- process trees at configured total process counts;
- an established loopback TCP connection;
- external TCP, explicitly classified unsupported without attempting a misleading local-only test;
- workspace mutation that must continue after restore.

Memory sizes are configurable for every in-scope scenario. Process counts are configurable for the
process-tree scenario. The fixture touches every allocated page before becoming ready. A row becomes
`supported` only after CRIU dump and restore both succeed, the workload's observable heartbeat and
scenario-specific behavior advance, image bytes are counted, and the restored process group is
removed. Tool absence, host incompatibility, timeout, command failure, verification failure, and
cleanup failure remain distinct structured results. No row is omitted because another row failed.

The runner uses private temporary roots, literal executable arguments, cleared child environments,
bounded memory/process matrices, bounded timeouts, bounded retained command output, and process-group
cleanup. Its evidence writer creates a new mode-0700 directory and refuses to overwrite an existing
path.

## Build and run

Build both the runner and its fixture:

```bash
cargo build -p epoch-criu-compat --bins --locked
```

On macOS the same binary writes a complete structured-Unsupported matrix without attempting CRIU.
On Linux, CRIU generally requires explicit privileges. This command writes JSON, Markdown, and
linked logs to a path that must not exist yet:

```bash
sudo target/debug/epoch-criu-compat \
  --output /var/tmp/epoch-criu-evidence \
  --criu /usr/local/sbin/criu \
  --fixture "$PWD/target/debug/epoch-criu-fixture" \
  --memory-bytes 4194304,67108864 \
  --process-counts 2,4
```

Run the real-kernel test explicitly; ordinary test runs skip it:

```bash
sudo env \
  HOME="$HOME" \
  RUSTUP_HOME="$HOME/.rustup" \
  CARGO_HOME="$HOME/.cargo" \
  CARGO_TARGET_DIR=/opt/epoch-criu-target \
  EPOCH_RUN_PRIVILEGED_CRIU=1 \
  EPOCH_CRIU_PATH=/usr/local/sbin/criu \
  "$HOME/.cargo/bin/cargo" test \
    -p epoch-criu-compat --test linux_native --locked -- --nocapture
```

## Oracle ARM64 environment

Ubuntu 24.04 did not offer an ARM64 `criu` package through the configured repositories on the test
date. CRIU 4.2 was therefore built from upstream tag `v4.2` at commit
`3c7d4fa013297b431da48eff821db7f2e8b90c27` using the dependencies listed by CRIU's build system and
installed root-owned at `/usr/local/sbin/criu`. The relevant dependency set was:

```text
build-essential git pkg-config protobuf-c-compiler libprotobuf-c-dev
libprotobuf-dev protobuf-compiler python3-protobuf libnl-3-dev libcap-dev
uuid-dev libaio-dev python3-yaml libnet-dev libgnutls28-dev libnftables-dev
libbsd-dev libselinux1-dev libbpf-dev
```

The host was Ubuntu 24.04 ARM64 with kernel `6.17.0-1018-oracle`. Basic `criu check` succeeded.
Extended `criu check --all` exited 1 and reported that dirty tracking was off, direct vDSO mapping
was unavailable, and compatible-task support was not compiled. Those warnings are preserved in the
[extended-check log](evidence/criu-oracle-arm64/logs/environment-criu-check-all.log); they do not
erase successful scenario-level evidence.

## Historical evidence and decision

The committed [machine-readable report](evidence/criu-oracle-arm64/compatibility.json),
[Markdown matrix](evidence/criu-oracle-arm64/compatibility.md), and linked logs are a historical
native run generated from source revision `96febca`. The older evidence schema did not embed its
source revision, so this directory is a baseline and must not be presented as final candidate
proof. All 12 declared in-scope rows restored and resumed correctly across 4 MiB and 64 MiB
resident allocations and two- and four-process trees. External TCP is retained as the thirteenth,
explicitly unsupported row.

Observed dump latency was 20–248 ms, restore latency was 10–124 ms, and image bytes ranged from
about 4.3 MiB to 67.6 MiB. These samples satisfy the preliminary 1-second checkpoint and 3-second
restore gates. The recommendation is therefore **keep the narrow declared CRIU subset**, not “use
CRIU for arbitrary agents.” Broader use remains narrowed until threads, pipes, Unix sockets, timers,
signals, deleted/backing-file changes, real external connections, repeated correctness runs, and
larger scaling points have their own evidence.

Final acceptance uses a new directory produced from a clean candidate revision. Its report must
embed the full 40-character `code_revision` and `code_dirty: false`, as specified by the
[final acceptance runbook](final-runbook.md).

The final Oracle ARM64 matrix for clean revision
`11739a8e3f2da9673d642ac718cee8bbb39dc229`, including all raw row logs and checksums, is indexed in
the [final candidate evidence](../results/final-11739a8/README.md). Twelve in-scope rows restored
and verified; external TCP remains the single explicit unsupported row.

## Integration contract and limitations

The supervisor integration seam is `RunnerConfig` plus `CompatibilityRunner::run`, which returns a
versioned `CompatibilityEvidence`/`CompatibilityReport`. Production integration should consume the
structured row status and diagnostics; it must not infer backend support from tool presence or from
basic `criu check` alone. The standalone binary intentionally does not modify the active Epoch CLI.

This prototype runs the fixture directly on the privileged validation host. It does not yet compose
restore with the Linux sandbox backend, persist CRIU images as Epoch checkpoint components, or
coordinate an atomic composite epoch. It also does not implement incremental/pre-dump mode,
cross-host migration, DLP, secret brokering, a dashboard, or live agent-provider calls. CRIU and the
host kernel remain in the trusted computing base.
