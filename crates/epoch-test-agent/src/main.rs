use std::{io::Write as _, path::PathBuf, process::ExitCode};

use clap::Parser;
use epoch_test_agent::{
    CrashPoint, DEFAULT_MEMORY_BYTES, Scenario, WorkloadConfig, WorkloadError, run_workload,
};

#[derive(Debug, Parser)]
#[command(
    name = "epoch-test-agent",
    about = "Deterministic boundary workload for Epoch runtime experiments"
)]
struct Cli {
    #[arg(long, default_value_t = 1)]
    seed: u64,
    #[arg(long, value_enum, default_value = "full")]
    scenario: Scenario,
    #[arg(long, default_value = ".epoch/workload")]
    workspace: PathBuf,
    #[arg(long, default_value_t = DEFAULT_MEMORY_BYTES)]
    memory_bytes: usize,
    #[arg(long, value_enum)]
    crash_at: Option<CrashPoint>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut config = WorkloadConfig::new(cli.seed, cli.scenario, cli.workspace);
    config.memory_bytes = cli.memory_bytes;
    config.crash_at = cli.crash_at;

    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    match run_workload(&config, &mut output) {
        Ok(summary) => write_summary(&summary),
        Err(error) => {
            eprintln!("{error}");
            if matches!(error, WorkloadError::InjectedCrash { .. }) {
                ExitCode::from(70)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn write_summary(summary: &epoch_test_agent::RunSummary) -> ExitCode {
    let stderr = std::io::stderr();
    let mut diagnostics = stderr.lock();
    if let Err(error) = serde_json::to_writer(&mut diagnostics, summary)
        .and_then(|()| diagnostics.write_all(b"\n").map_err(serde_json::Error::io))
    {
        eprintln!("failed to encode deterministic run summary: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
