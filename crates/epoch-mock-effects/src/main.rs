use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use epoch_mock_effects::MockEffectServer;

#[derive(Debug, Parser)]
#[command(
    name = "epoch-mock-effects",
    version,
    about = "Durable idempotent mock email and payment service"
)]
struct Arguments {
    /// `SQLite` database used as the mock remote system of record.
    #[arg(long, default_value = ".epoch/mock-effects.db")]
    database: PathBuf,
    /// Local address to listen on.
    #[arg(long, default_value = "127.0.0.1:8081")]
    bind: String,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let mut server = match MockEffectServer::bind(&arguments.bind, &arguments.database) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("failed to start mock effect service: {error}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "mock effect service listening on http://{}",
        server.local_addr()
    );
    if let Err(error) = server.serve_forever() {
        eprintln!("mock effect service stopped: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_defaults_to_loopback() {
        let arguments = Arguments::try_parse_from(["epoch-mock-effects"]).expect("parse defaults");
        assert_eq!(arguments.bind, "127.0.0.1:8081");
        assert_eq!(arguments.database, PathBuf::from(".epoch/mock-effects.db"));
    }
}
