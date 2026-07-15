# Final Week 4 acceptance runbook

This is the single acceptance path for the interview prototype. Run it from a clean checkout of
the candidate revision. Evidence is valid only when its embedded `code_revision` matches the
tested commit and `code_dirty` is false. Dashboard, OpenCode/DeepSeek, live provider calls, DLP,
and PII/NER work are intentionally outside this gate.

## 1. Cross-platform correctness gate

```bash
cargo fmt --all -- --check
cargo test --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
git diff --check
git status --short
```

The final two commands must produce no output.

## 2. Thirteen-phase demo and three rehearsals

```bash
cargo build --workspace --bins --locked
DEMO_PARENT="$(mktemp -d)"
for rehearsal in 1 2 3; do
  root="$DEMO_PARENT/rehearsal-$rehearsal"
  ./target/debug/epoch demo \
    --agent "$PWD/target/debug/epoch-test-agent" \
    --root "$root" \
    --workspace "$root/workspaces" \
    --json > "$DEMO_PARENT/rehearsal-$rehearsal.json"
done
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$DEMO_PARENT"/rehearsal-*.json
else
  shasum -a 256 "$DEMO_PARENT"/rehearsal-*.json
fi
```

Each report must say `13/13`, contain 13 succeeded phases, identify the clean candidate revision,
show `workspace_restored: true`, verify restored checkpoint bytes, retain composite semantic
workspace/capability/effect evidence, and list only continuation, live-provider effects, the Linux
supervisor adapter, and process memory as unsupported.

## 3. Frozen bounded benchmark and report reload

```bash
BENCH_ROOT="$PWD/.epoch/final-benchmarks"
./target/debug/epoch bench run all \
  --root "$BENCH_ROOT" \
  --warmups 2 \
  --repetitions 10 \
  --fixture-bytes 1048576 \
  --fixture-files 16 \
  --seed 24301 \
  --cow-allocation-bytes 67108864 \
  --cow-children 4 \
  --cow-dirty-basis-points 2500 \
  --cow-repetitions 5 \
  --performance-repetitions 3 \
  --isolation-repetitions 5 \
  --performance-max-memory-bytes 4294967296

./target/debug/epoch bench report <emitted-run-id> \
  --root "$BENCH_ROOT" \
  --format markdown
```

The JSON, CSV, and Markdown artifacts are the same evidence in three stable views. The `all` run
must include real checkpoint/restore samples, compatibility rows, all 60 final COW matrix keys,
direct-vs-Linux isolation rows, workspace fault injection, 100 effect replays with exactly one
deterministic dispatch, unknown-effect branch suspension, and revocation/policy-resurrection
rejection. On non-Linux hosts the 60 COW keys and Linux isolation row remain structured unsupported,
not omitted. A deterministic local dispatcher proves the runtime protocol; it does not prove a
live provider's commit semantics.

## 4. Privileged Oracle ARM64 gates

Use an isolated checkout of the exact candidate commit. Build as the normal user, then preserve
the user's Rust toolchain paths when invoking only the explicitly privileged tests.

```bash
cargo build --workspace --bins --locked
cargo test --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

sudo install -d -o root -g root -m 0755 /usr/local/libexec
sudo install -o root -g root -m 0755 \
  target/debug/epoch-sandbox-init \
  /usr/local/libexec/epoch-sandbox-init
sudo install -o root -g root -m 0755 \
  target/debug/epoch-performance-probe \
  /usr/local/libexec/epoch-performance-probe
sudo install -d -m 0777 /var/tmp/epoch-performance-workspace

sudo env \
  PATH=/home/ubuntu/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  GIT_CONFIG_COUNT=1 \
  GIT_CONFIG_KEY_0=safe.directory \
  GIT_CONFIG_VALUE_0="$PWD" \
  ./target/debug/epoch bench run all \
  --root /var/tmp/epoch-final-linux-benchmarks \
  --warmups 2 \
  --repetitions 10 \
  --fixture-bytes 1048576 \
  --fixture-files 16 \
  --seed 24301 \
  --cow-allocation-bytes 67108864 \
  --cow-children 4 \
  --cow-dirty-basis-points 2500 \
  --cow-repetitions 5 \
  --performance-repetitions 3 \
  --isolation-repetitions 7 \
  --performance-max-memory-bytes 4294967296 \
  --performance-sandbox-helper /usr/local/libexec/epoch-sandbox-init \
  --performance-probe /usr/local/libexec/epoch-performance-probe \
  --performance-workspace /var/tmp/epoch-performance-workspace

sudo env \
  HOME=/home/ubuntu \
  RUSTUP_HOME=/home/ubuntu/.rustup \
  CARGO_HOME=/home/ubuntu/.cargo \
  CARGO_TARGET_DIR=/opt/epoch-root-isolation-target \
  EPOCH_RUN_PRIVILEGED_ISOLATION=1 \
  /home/ubuntu/.cargo/bin/cargo test \
    -p epoch-sandbox --test linux_native --locked -- --nocapture

sudo env \
  HOME=/home/ubuntu \
  RUSTUP_HOME=/home/ubuntu/.rustup \
  CARGO_HOME=/home/ubuntu/.cargo \
  CARGO_TARGET_DIR=/opt/epoch-criu-target \
  EPOCH_RUN_PRIVILEGED_CRIU=1 \
  EPOCH_CRIU_PATH=/usr/local/sbin/criu \
  /home/ubuntu/.cargo/bin/cargo test \
    -p epoch-criu-compat --test linux_native --locked -- --nocapture
```

Persist the standalone CRIU matrix to a new directory:

```bash
sudo ./target/debug/epoch-criu-compat \
  --output /var/tmp/epoch-criu-final \
  --criu /usr/local/sbin/criu \
  --fixture "$PWD/target/debug/epoch-criu-fixture" \
  --memory-bytes 4194304,67108864 \
  --process-counts 2,4
```

The CRIU report must contain all 13 declared rows: 12 attempted in-scope rows across the frozen
memory/process axes and the explicit unsupported external-TCP row. A `narrow` result is expected:
successful declared rows do not establish support for arbitrary processes or cross-host migration.

## 5. Evidence preservation

Copy only final candidate artifacts into a revision-named directory. Preserve raw JSON/CSV/logs,
then generate checksums without rewriting the measured files:

```bash
if command -v sha256sum >/dev/null 2>&1; then
  find <evidence-directory> -type f ! -name SHA256SUMS -print0 \
    | sort -z | xargs -0 sha256sum > <evidence-directory>/SHA256SUMS
else
  find <evidence-directory> -type f ! -name SHA256SUMS -print0 \
    | sort -z | xargs -0 shasum -a 256 > <evidence-directory>/SHA256SUMS
fi
```

Older committed Oracle artifacts are historical baselines, not final proof. Their documentation
must name the older source revision explicitly.

## Keep / narrow / kill

| Decision | Scope | Evidence rule |
|---|---|---|
| Keep | Cooperative application checkpoint plus immutable no-clobber workspace restore | Correctness validations pass and capture/restore p95 stays within the frozen thresholds |
| Keep | Monotonic capabilities and durable effect gateway | One hundred stable replays dispatch once; ambiguous outcomes become unknown and suspend; revocation and policy rollback cannot resurrect authority |
| Keep | Semantic diff and durable fork lineage | Restart-safe inspection and composite workspace/security frontiers remain observable |
| Narrow | Linux isolation | Native namespace/cgroup/seccomp tests pass, but the CLI supervisor launch adapter is not composed |
| Narrow | Fork/COW memory optimization | Matrix evidence is workload- and kernel-specific; it is not a process checkpoint |
| Narrow | CRIU | Only declared compatible rows are supported; external TCP and unmeasured kernel resources stay unsupported |
| Kill | Transparent arbitrary-process checkpoint claims | Compatibility is not universal and unsupported rows must never become false success |
| Kill | "Exactly once" from rollback alone | Live provider commit/reconciliation semantics are outside local rollback and must remain explicit |

## Interview Q&A / known issues

- **Does restore rewind the original directory?** No. It publishes verified checkpoint bytes to a
  new target and refuses to clobber existing content.
- **Does a checkpoint restore process memory?** No. Application context and workspace state are
  composite components; CRIU remains a separate compatibility experiment.
- **Can revoked authority return after restore?** No. Capability and policy state are monotonic,
  trusted, append-audited state outside rollback.
- **Can an ambiguous email/API call retry automatically?** No. The effect becomes `unknown`, the
  branch suspends, and retry is blocked until a provider-specific reconciler resolves it.
- **Is Linux isolation production-wired?** The backend is native-tested and fails closed, but the
  supervisor adapter is intentionally not composed yet.
- **Why no live LLM or email demo?** The deterministic agent removes credentials, network variance,
  cost, and provider nondeterminism from architecture evidence.
- **Why are some conclusions narrow?** One kernel, workload, or compatibility matrix cannot justify
  a universal runtime claim. Unsupported and failed rows stay in the artifacts.
