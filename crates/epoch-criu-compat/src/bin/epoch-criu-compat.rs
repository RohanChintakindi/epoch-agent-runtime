//! Standalone CRIU compatibility evidence runner.

use std::path::PathBuf;

use clap::Parser;
use epoch_criu_compat::{CompatibilityRunner, RunLimits, RunnerConfig, ScalingPlan};

#[derive(Debug, Parser)]
#[command(name = "epoch-criu-compat")]
#[command(about = "Run bounded CRIU compatibility experiments into a new evidence directory")]
struct Arguments {
    /// New directory for stable JSON, Markdown, and bounded logs.
    #[arg(long)]
    output: PathBuf,

    /// Absolute CRIU executable path.
    #[arg(long, default_value = "/usr/local/sbin/criu")]
    criu: PathBuf,

    /// Absolute compatibility fixture path; defaults to the sibling built binary.
    #[arg(long)]
    fixture: Option<PathBuf>,

    /// Comma-separated resident memory sizes tested for each in-scope scenario.
    #[arg(long, value_delimiter = ',', default_value = "4194304,67108864")]
    memory_bytes: Vec<u64>,

    /// Comma-separated total process counts tested by the process-tree scenario.
    #[arg(long, value_delimiter = ',', default_value = "2,4")]
    process_counts: Vec<u32>,

    #[arg(long, default_value_t = 30_000)]
    dump_timeout_ms: u64,

    #[arg(long, default_value_t = 30_000)]
    restore_timeout_ms: u64,

    #[arg(long, default_value_t = 262_144)]
    max_log_bytes: usize,
}

fn main() {
    if let Err(message) = run() {
        eprintln!("epoch-criu-compat: {message}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let arguments = Arguments::parse();
    let fixture = arguments.fixture.map_or_else(default_fixture_path, Ok)?;
    let limits = RunLimits::new(
        arguments.dump_timeout_ms,
        arguments.restore_timeout_ms,
        arguments.max_log_bytes,
    )
    .map_err(|error| error.to_string())?;
    let scaling = ScalingPlan::new(arguments.memory_bytes, arguments.process_counts)
        .map_err(|error| error.to_string())?;
    let config = RunnerConfig::new(arguments.criu, fixture, limits, scaling)
        .map_err(|error| error.to_string())?;
    let evidence = CompatibilityRunner::new(config)
        .run()
        .map_err(|error| error.to_string())?;
    evidence
        .write_new(&arguments.output)
        .map_err(|error| error.to_string())?;
    println!("evidence={}", arguments.output.display());
    Ok(())
}

fn default_fixture_path() -> Result<PathBuf, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot resolve runner executable: {error}"))?;
    let parent = executable
        .parent()
        .ok_or_else(|| "runner executable has no parent directory".to_owned())?;
    let mut fixture = parent.join("epoch-criu-fixture");
    if cfg!(windows) {
        fixture.set_extension("exe");
    }
    Ok(fixture)
}
