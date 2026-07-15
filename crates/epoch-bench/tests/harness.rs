use std::collections::BTreeMap;

use epoch_bench::{
    BenchmarkConfig, BenchmarkEnvironment, BenchmarkHarness, BenchmarkScenario, ConfigError,
    Iteration, PercentileSummary, SampleMeasurement, SampleOutcome, TraceMode,
};

#[derive(Default)]
struct FixtureScenario {
    iterations: Vec<Iteration>,
}

impl BenchmarkScenario for FixtureScenario {
    fn measure(&mut self, iteration: Iteration) -> SampleMeasurement {
        self.iterations.push(iteration.clone());
        let outcome = match iteration.ordinal {
            1 if !iteration.warmup => SampleOutcome::Unsupported {
                reason: "backend unavailable".to_owned(),
            },
            2 if !iteration.warmup => SampleOutcome::Failed {
                error: "injected failure".to_owned(),
            },
            _ => SampleOutcome::Succeeded,
        };
        SampleMeasurement {
            elapsed_ns: u64::from(iteration.ordinal + 1) * 10,
            outcome,
            bytes_read: Some(u64::from(iteration.ordinal)),
            bytes_written: None,
            metrics: BTreeMap::from([("faults".to_owned(), f64::from(iteration.ordinal))]),
        }
    }
}

fn environment() -> BenchmarkEnvironment {
    BenchmarkEnvironment {
        code_revision: "abc123".to_owned(),
        os: "linux".to_owned(),
        architecture: "aarch64".to_owned(),
        kernel_release: "6.17.0-test".to_owned(),
        cpu_count: 4,
        total_memory_bytes: Some(8 * 1024 * 1024 * 1024),
        runtime_version: "epoch-test".to_owned(),
        extra: BTreeMap::from([("host_class".to_owned(), "oracle".to_owned())]),
    }
}

#[test]
fn runs_warmups_then_retains_every_measured_outcome() {
    let config = BenchmarkConfig::new("checkpoint", "application", TraceMode::Off, 7, 2, 4)
        .expect("valid config");
    let mut scenario = FixtureScenario::default();

    let report = BenchmarkHarness::run(config, environment(), &mut scenario);

    assert_eq!(scenario.iterations.len(), 6);
    assert!(scenario.iterations[..2].iter().all(|item| item.warmup));
    assert!(scenario.iterations[2..].iter().all(|item| !item.warmup));
    assert_eq!(report.samples.len(), 4);
    assert_eq!(report.summary.succeeded, 2);
    assert_eq!(report.summary.unsupported, 1);
    assert_eq!(report.summary.failed, 1);
    assert_eq!(report.summary.latency_ns.sample_count, 2);
    assert_ne!(scenario.iterations[0].seed, scenario.iterations[1].seed);
}

#[test]
fn rejects_ambiguous_or_unbounded_configurations() {
    assert_eq!(
        BenchmarkConfig::new("", "direct", TraceMode::Off, 1, 0, 1),
        Err(ConfigError::EmptySuite)
    );
    assert_eq!(
        BenchmarkConfig::new("run", "", TraceMode::Off, 1, 0, 1),
        Err(ConfigError::EmptyBackend)
    );
    assert_eq!(
        BenchmarkConfig::new("run", "direct", TraceMode::Off, 1, 0, 0),
        Err(ConfigError::InvalidRepetitions { value: 0 })
    );
    assert!(matches!(
        BenchmarkConfig::new("run", "direct", TraceMode::Off, 1, u32::MAX, 1),
        Err(ConfigError::InvalidWarmups { .. })
    ));
}

#[test]
fn percentile_summary_uses_nearest_rank_over_successful_samples() {
    assert_eq!(
        PercentileSummary::from_sorted(&[10, 20, 30, 40]),
        PercentileSummary {
            sample_count: 4,
            min: Some(10),
            p50: Some(20),
            p95: Some(40),
            p99: Some(40),
            max: Some(40),
        }
    );
    assert_eq!(
        PercentileSummary::from_sorted(&[]),
        PercentileSummary {
            sample_count: 0,
            min: None,
            p50: None,
            p95: None,
            p99: None,
            max: None,
        }
    );
}

#[test]
fn serialization_is_stable_and_csv_keeps_failures_and_unsupported_rows() {
    let config = BenchmarkConfig::new("checkpoint", "application", TraceMode::On, 99, 0, 4)
        .expect("valid config");
    let report = BenchmarkHarness::run(config, environment(), &mut FixtureScenario::default());

    let first = report.to_json().expect("serialize report");
    assert_eq!(first, report.to_json().expect("serialize repeatedly"));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&first).unwrap()["schema_version"], 1);

    let csv = report.to_csv();
    assert_eq!(csv.lines().count(), 5);
    assert!(csv.contains("unsupported"));
    assert!(csv.contains("failed"));
    assert!(csv.contains("backend unavailable"));
}

#[test]
fn trace_mode_is_part_of_the_authoritative_configuration() {
    let off = BenchmarkConfig::new("run", "direct", TraceMode::Off, 1, 0, 1).unwrap();
    let on = BenchmarkConfig::new("run", "direct", TraceMode::On, 1, 0, 1).unwrap();

    assert_ne!(off, on);
    assert_ne!(
        BenchmarkHarness::run(off, environment(), &mut FixtureScenario::default()).to_json(),
        BenchmarkHarness::run(on, environment(), &mut FixtureScenario::default()).to_json()
    );
}
