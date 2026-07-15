# Linux sandbox prototype

`epoch-sandbox` answers a narrow architecture question: can Epoch put an arbitrary agent process
behind useful Linux boundaries with startup latency measured in tens of milliseconds, while keeping
unsupported hosts and setup failures explicit? It does not claim VM-strength isolation.

## Boundary and launch sequence

The supervisor chooses either the explicit `direct` baseline or the `linux` backend. Linux backend
discovery requires Linux, effective UID 0, cgroup v2 `cpu`, `memory`, and `pids` controllers,
`systemd-run`, Bubblewrap, `systemctl`, and an `aarch64` or `x86_64` seccomp profile. A missing
facility produces a structured `Unsupported` result. It never launches the workload directly as a
fallback.

For a supported request, the backend:

1. validates and canonicalizes the executable, trusted helper, workspace, working directory,
   literal arguments, bounded resource limits, and a small non-secret `EPOCH_*` environment;
2. creates a collected transient systemd scope with `MemoryMax`, `TasksMax`, and `CPUQuota`;
3. enters Bubblewrap user, PID, network, IPC, UTS, and cgroup namespaces without a shell;
4. exposes `/` read-only, mounts private `/tmp` and `/run`, remounts only the workspace writable,
   and read-only binds the helper and workload at fixed in-sandbox paths;
5. switches to UID/GID 65534 and drops every capability;
6. clears the environment, installs the architecture-specific seccomp-v1 allowlist with
   `no_new_privs`, and finally calls `execve` for the workload.

Lifecycle operations act on the whole transient scope. Cleanup stops a live scope, reaps the
launcher, and relies on `--collect` to remove the empty unit.

## Threat model

Assume the workload and everything writable in its workspace are hostile. The workload may execute
arbitrary code, fork, consume resources up to configured limits, and corrupt or delete its own
workspace. The prototype aims to prevent it from writing the host base filesystem, observing host
processes, using the host network, acquiring capabilities, creating another user namespace, or
escaping its cgroup limits. Launch arguments are passed literally; no shell interpolation occurs.
Provider credentials are rejected from the sandbox environment rather than treated as isolatable
secrets.

The trusted computing base includes the host kernel, systemd, Bubblewrap, the root-owned
`epoch-sandbox-init` binary, its embedded seccomp profile, and the privileged supervisor. The helper
must be owned by root and not group- or world-writable. Capability discovery is compatibility
checking, not proof that the kernel or those trusted components are uncompromised.

## Current limitations

- This is process isolation, not a VM boundary. A Linux kernel vulnerability remains an escape path.
- The read-only host root exposes files that UID 65534 may read. It prevents mutation but is not a
  confidentiality boundary for world-readable host data. A production image should replace this
  bind with a minimal immutable root filesystem.
- The network namespace has no interfaces except loopback state created by the kernel; there is no
  selective egress proxy yet.
- Seccomp-v1 is a compatibility-oriented allowlist for the deterministic Rust workload. Other
  runtimes may fail with exit code 125 or a denied syscall and need a reviewed profile change.
- The workspace must be traversable and writable by UID/GID 65534. An incompatible workspace fails
  at launch; the prototype does not change caller-owned filesystem ownership.
- Discovery currently requires an explicitly privileged supervisor because the Oracle validation
  host blocks unprivileged namespace creation. There is no setuid helper and no implicit privilege
  escalation.
- macOS reports `PlatformNotLinux`; it only compiles and exercises the backend contract.
- The current supervisor/CLI product path has not selected this backend yet. The crate is the tested
  integration seam; `direct` remains the existing product default until that selection is wired.

CRIU, microVMs, checkpointing, replay, DLP, and secret brokering are separate concerns and are not
implemented by this crate.

## Reproduce the gates

Portable contract gate:

```bash
cargo test -p epoch-sandbox --locked
cargo clippy -p epoch-sandbox --all-targets --locked -- -D warnings
```

Privileged native gate on the documented Ubuntu ARM64 host, with a root-owned Cargo target under a
path traversable by UID 65534:

```bash
sudo env \
  HOME="$HOME" \
  RUSTUP_HOME="$HOME/.rustup" \
  CARGO_HOME="$HOME/.cargo" \
  CARGO_TARGET_DIR=/opt/epoch-root-isolation-target \
  EPOCH_RUN_PRIVILEGED_ISOLATION=1 \
  "$HOME/.cargo/bin/cargo" test -p epoch-sandbox --test linux_native --locked -- --nocapture
```

The native suite verifies filesystem and namespace boundaries, capability and seccomp state,
private temporary storage, PID and memory enforcement, unit collection, and direct-versus-sandbox
launch samples. A validation run on Ubuntu 24.04 ARM64 measured direct launches at about
0.25–0.96 ms and Linux sandbox launches at about 22–32 ms. These are smoke-test samples, not a
statistically rigorous benchmark.
