# Cross-language ML smoke

`scripts/ml-cross-language-smoke.sh` is the fail-fast acceptance path for the Rust trajectory
exporter and the offline Python branch-value package. It resolves the repository root from its own
location, so it can be launched from any working directory:

```bash
/path/to/epoch/scripts/ml-cross-language-smoke.sh
```

The script builds `epoch`, creates a private temporary root, writes a credential-free deterministic
shell agent and workload manifest, runs the agent, extracts the session ID without `jq`, and exports
that session through `epoch ml export`. The exact Rust JSONL is then passed to the locked Python
reader. It proceeds only if that wire contract is accepted.

After the cross-language check, the script runs the bounded synthetic workflow:

1. deterministic generation;
2. one-epoch CPU training;
3. test-split evaluation;
4. advisory scoring of both the synthetic data and the exact Rust export with
   `score DATASET --model-dir DIR --output FILE`.

It checks every expected artifact and proves that Rust export, model training, and prediction
scoring refuse to clobber existing output. It also requires private file and directory modes and
the model bundle's integrity manifest. A guarded exit trap removes the private temporary root on
success, failure, or interruption.

This smoke requires both ML integration contracts: the Python reader must implement the Rust
trajectory schema in `docs/trajectory-schema.md`, and the package must expose the advisory `score`
command. On a partial integration branch, rejection at the exact Rust JSONL validation or the
missing `score` command is an expected red gate, not a reason to weaken the smoke.
