//! Final bounded COW and isolation performance evidence.

mod artifacts;
mod cow;
mod environment;
mod isolation;
mod model;

pub use artifacts::write_artifacts;
pub use cow::{plan_cow_matrix, run_cow_matrix};
pub use environment::discover_environment;
pub use isolation::{run_isolation_comparison, summarize_backend, unsupported_linux_comparison};
pub use model::*;

/// Executes both final performance matrices with failures retained as evidence rows.
pub struct PerformanceRunner {
    config: PerformanceConfig,
    environment: BenchmarkEnvironment,
}

impl PerformanceRunner {
    #[must_use]
    pub fn new(config: PerformanceConfig, mut environment: BenchmarkEnvironment) -> Self {
        environment.code_revision.clone_from(&config.code_revision);
        Self {
            config,
            environment,
        }
    }

    #[must_use]
    pub fn run(&self) -> PerformanceReport {
        PerformanceReport {
            schema_version: SCHEMA_VERSION,
            config: self.config.clone(),
            environment: self.environment.clone(),
            cow: run_cow_matrix(&self.config.cow, &self.environment),
            isolation: run_isolation_comparison(&self.config.isolation),
        }
    }
}
