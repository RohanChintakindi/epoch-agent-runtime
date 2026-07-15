use std::path::PathBuf;

use clap::Parser;
use epoch_performance_matrix::{
    CowMatrixConfig, IsolationConfig, PerformanceConfig, PerformanceRunner, discover_environment,
    write_artifacts,
};

#[derive(Parser)]
#[command(about = "Generate bounded COW and direct-vs-Linux isolation evidence")]
struct Arguments {
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    code_revision: String,
    #[arg(long, default_value_t = 3)]
    repetitions: u16,
    #[arg(long, default_value_t = 5)]
    isolation_repetitions: u16,
    #[arg(long, default_value_t = 4 * 1024 * 1024 * 1024_u64)]
    max_memory_bytes: u64,
    #[arg(long)]
    include_optional_2gib: bool,
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/helpers/cow_matrix_probe.py"))]
    cow_helper: PathBuf,
    #[arg(long, default_value = "/usr/bin/python3")]
    python: PathBuf,
    #[arg(long)]
    isolation_probe: Option<PathBuf>,
    #[arg(long)]
    sandbox_helper: Option<PathBuf>,
    #[arg(long)]
    isolation_workspace: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("epoch-performance-matrix: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = Arguments::parse();
    let environment = discover_environment(&arguments.code_revision, arguments.max_memory_bytes)?;
    let mut cow = CowMatrixConfig::required();
    cow.repetitions = arguments.repetitions;
    cow.helper = Some(arguments.cow_helper);
    cow.python = arguments.python;
    if arguments.include_optional_2gib {
        cow = cow.include_optional_2gib();
    }
    let isolation = IsolationConfig {
        repetitions: arguments.isolation_repetitions,
        probe: arguments.isolation_probe,
        trusted_sandbox_helper: arguments.sandbox_helper,
        workspace: arguments.isolation_workspace,
        ..IsolationConfig::disabled_fixture()
    };
    let report = PerformanceRunner::new(
        PerformanceConfig {
            code_revision: arguments.code_revision,
            cow,
            isolation,
        },
        environment,
    )
    .run();
    let bundle = write_artifacts(&arguments.output, &report)?;
    println!("{}", bundle.json.display());
    Ok(())
}
