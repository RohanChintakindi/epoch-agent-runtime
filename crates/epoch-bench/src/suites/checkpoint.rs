use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    str::FromStr as _,
    time::Instant,
};

use epoch_blob::{BlobHash, BlobStore};
use epoch_checkpoint::{
    APPLICATION_CONTEXT_SCHEMA_VERSION, ApplicationCheckpointBackend, ApplicationContext,
    BackendOutcome, CheckpointBackend, MessageRole, ObservableMessage, ResumeCursors,
};
use epoch_workspace::{WorkspaceBackend, WorkspaceError, WorkspaceLimits};
use uuid::Uuid;

use crate::{
    BenchmarkConfig, BenchmarkEnvironment, BenchmarkHarness, BenchmarkScenario, Iteration,
    SampleMeasurement, SampleOutcome, TraceMode,
};

use super::{
    CheckpointSuiteConfig, CheckpointSuiteEvidence, CompatibilityMatrix, CompatibilityRow,
    SuiteError, ValidationCase, bounded,
};

const TRACE_RECORDS: u32 = 64;

struct CombinedScenario {
    root: PathBuf,
    fixture_bytes: u64,
    fixture_files: u32,
    trace_mode: TraceMode,
}

struct SuccessfulMeasurement {
    elapsed_ns: u64,
    bytes: u64,
    metrics: BTreeMap<String, f64>,
}

#[derive(Clone, Copy)]
struct CombinedTimings {
    application_capture: u64,
    workspace_capture: u64,
    application_restore: u64,
    workspace_validation: u64,
    workspace_restore: u64,
}

impl BenchmarkScenario for CombinedScenario {
    fn measure(&mut self, iteration: Iteration) -> SampleMeasurement {
        let started = Instant::now();
        match self.measure_inner(&iteration) {
            Ok(measurement) => SampleMeasurement {
                elapsed_ns: measurement.elapsed_ns,
                outcome: SampleOutcome::Succeeded,
                bytes_read: Some(measurement.bytes),
                bytes_written: Some(measurement.bytes),
                metrics: measurement.metrics,
            },
            Err(error) => SampleMeasurement {
                elapsed_ns: elapsed_ns(started),
                outcome: SampleOutcome::Failed {
                    error: bounded(&error.to_string()),
                },
                bytes_read: None,
                bytes_written: None,
                metrics: BTreeMap::new(),
            },
        }
    }
}

impl CombinedScenario {
    fn measure_inner(&self, iteration: &Iteration) -> Result<SuccessfulMeasurement, SuiteError> {
        let sample_root = self.root.join(format!(
            "{}-{}-{}",
            if iteration.warmup { "warmup" } else { "sample" },
            iteration.ordinal,
            iteration.seed
        ));
        let source = sample_root.join("source");
        let blob_root = sample_root.join("blobs");
        let restore_target = sample_root.join("restored");
        fs::create_dir_all(&source)?;
        write_workspace(
            &source,
            self.fixture_bytes,
            self.fixture_files,
            iteration.seed,
        )?;
        let context = application_context(&blob_root, iteration.seed, self.trace_mode)?;
        let application = ApplicationCheckpointBackend::new(
            BlobStore::open(&blob_root).map_err(|error| SuiteError::Backend(error.to_string()))?,
        );
        let workspace = WorkspaceBackend::open(&blob_root, WorkspaceLimits::default())
            .map_err(|error| SuiteError::Backend(error.to_string()))?;

        let total_started = Instant::now();
        let application_started = Instant::now();
        let application_checkpoint = supported_checkpoint(application.capture(&context))?;
        let application_capture_ns = elapsed_ns(application_started);
        let workspace_started = Instant::now();
        let workspace_checkpoint = workspace
            .snapshot(&source)
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        let workspace_capture_ns = elapsed_ns(workspace_started);

        let application_restore_started = Instant::now();
        let restored_context = supported_context(application.restore(&application_checkpoint))?;
        let application_restore_ns = elapsed_ns(application_restore_started);
        if restored_context != context {
            return Err(SuiteError::Backend(
                "application restore did not reproduce the captured context".to_owned(),
            ));
        }
        let validation_started = Instant::now();
        let validation = workspace
            .validate(&workspace_checkpoint)
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        let workspace_validation_ns = elapsed_ns(validation_started);
        let workspace_restore_started = Instant::now();
        let restore = workspace
            .restore(&workspace_checkpoint, &restore_target)
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        let workspace_restore_ns = elapsed_ns(workspace_restore_started);
        if validation != restore || restore.total_file_bytes != self.fixture_bytes {
            return Err(SuiteError::Backend(
                "workspace restore validation disagreed with captured fixture".to_owned(),
            ));
        }

        let bytes = application_checkpoint
            .byte_length
            .saturating_add(workspace_checkpoint.manifest_length())
            .saturating_add(restore.total_file_bytes);
        let metrics = combined_metrics(
            CombinedTimings {
                application_capture: application_capture_ns,
                workspace_capture: workspace_capture_ns,
                application_restore: application_restore_ns,
                workspace_validation: workspace_validation_ns,
                workspace_restore: workspace_restore_ns,
            },
            application_checkpoint.byte_length,
            workspace_checkpoint.manifest_length(),
            restore.total_file_bytes,
            self.trace_mode,
        );
        Ok(SuccessfulMeasurement {
            elapsed_ns: elapsed_ns(total_started),
            bytes,
            metrics,
        })
    }
}

/// Runs real Week 2 application and workspace capture/validate/restore APIs for both trace modes.
///
/// # Errors
///
/// Returns a bounded configuration, setup, or unexpected supported-backend failure.
pub fn run_checkpoint_suite(
    config: &CheckpointSuiteConfig,
    environment: &BenchmarkEnvironment,
) -> Result<CheckpointSuiteEvidence, SuiteError> {
    config.validate()?;
    validate_environment(environment)?;
    let run_root = config.root.join(format!("checkpoint-{}", Uuid::new_v4()));
    fs::create_dir_all(&run_root)?;
    let mut reports = Vec::with_capacity(2);
    for trace_mode in [TraceMode::Off, TraceMode::On] {
        let benchmark_config = BenchmarkConfig::new(
            "application_workspace_checkpoint",
            "cooperative-v1+full-copy-cas-v1",
            trace_mode,
            config.seed,
            config.warmups,
            config.repetitions,
        )
        .map_err(|error| SuiteError::InvalidConfig(error.to_string()))?;
        let mut scenario = CombinedScenario {
            root: run_root.join(trace_mode.label()),
            fixture_bytes: config.fixture_bytes,
            fixture_files: config.fixture_files,
            trace_mode,
        };
        reports.push(BenchmarkHarness::run(
            benchmark_config,
            environment.clone(),
            &mut scenario,
        ));
    }
    let validation_cases = validation_cases(&run_root.join("validation"), config.seed)?;
    Ok(CheckpointSuiteEvidence {
        reports,
        validation_cases,
    })
}

fn combined_metrics(
    timings: CombinedTimings,
    application_bytes: u64,
    workspace_manifest_bytes: u64,
    workspace_file_bytes: u64,
    trace_mode: TraceMode,
) -> BTreeMap<String, f64> {
    BTreeMap::from([
        (
            "application_capture_ns".to_owned(),
            metric(timings.application_capture),
        ),
        (
            "workspace_capture_ns".to_owned(),
            metric(timings.workspace_capture),
        ),
        (
            "application_restore_ns".to_owned(),
            metric(timings.application_restore),
        ),
        (
            "workspace_validation_ns".to_owned(),
            metric(timings.workspace_validation),
        ),
        (
            "workspace_restore_ns".to_owned(),
            metric(timings.workspace_restore),
        ),
        (
            "application_checkpoint_bytes".to_owned(),
            metric(application_bytes),
        ),
        (
            "workspace_manifest_bytes".to_owned(),
            metric(workspace_manifest_bytes),
        ),
        (
            "workspace_file_bytes".to_owned(),
            metric(workspace_file_bytes),
        ),
        (
            "trace_records".to_owned(),
            f64::from(if trace_mode == TraceMode::On {
                TRACE_RECORDS
            } else {
                0
            }),
        ),
    ])
}

/// Runs scaling and compatibility cases without filtering unsupported or failed outcomes.
///
/// # Errors
///
/// Returns only suite setup/configuration failures; row failures remain inside the matrix.
pub fn run_compatibility_matrix(
    config: &CheckpointSuiteConfig,
    environment: BenchmarkEnvironment,
) -> Result<CompatibilityMatrix, SuiteError> {
    config.validate()?;
    validate_environment(&environment)?;
    let root = config
        .root
        .join(format!("compatibility-{}", Uuid::new_v4()));
    fs::create_dir_all(&root)?;
    let mut rows = Vec::new();
    for (index, bytes) in [4_096_u64, 65_536, 1_048_576].into_iter().enumerate() {
        rows.push(scaling_row(
            &root,
            u32::try_from(index).unwrap_or(u32::MAX),
            bytes,
            config.seed,
        ));
    }
    rows.push(future_schema_row(&root.join("future-schema"))?);
    rows.push(missing_reference_row(&root.join("missing-reference"))?);
    rows.push(missing_workspace_row(&root.join("missing-workspace"))?);
    rows.push(special_workspace_row(&root.join("special-workspace"))?);
    for case in [
        "threads",
        "child_processes",
        "pipes",
        "unix_sockets",
        "tcp_connections",
        "timers_signals",
    ] {
        rows.push(CompatibilityRow {
            case: format!("process_{case}"),
            component: "process_checkpoint".to_owned(),
            configuration: BTreeMap::from([("workload".to_owned(), case.to_owned())]),
            outcome: SampleOutcome::Unsupported {
                reason: "no process checkpoint backend is registered on this revision".to_owned(),
            },
            elapsed_ns: 0,
            evidence: BTreeMap::from([("backend".to_owned(), "unregistered".to_owned())]),
        });
    }
    Ok(CompatibilityMatrix { environment, rows })
}

fn scaling_row(root: &Path, ordinal: u32, bytes: u64, seed: u64) -> CompatibilityRow {
    let mut scenario = CombinedScenario {
        root: root.join(format!("scale-{bytes}")),
        fixture_bytes: bytes,
        fixture_files: 4,
        trace_mode: TraceMode::Off,
    };
    let measurement = scenario.measure(Iteration {
        ordinal,
        seed: seed.wrapping_add(u64::from(ordinal)),
        warmup: false,
    });
    CompatibilityRow {
        case: format!("combined_{bytes}_bytes"),
        component: "application_and_workspace".to_owned(),
        configuration: BTreeMap::from([
            ("fixture_bytes".to_owned(), bytes.to_string()),
            ("fixture_files".to_owned(), "4".to_owned()),
        ]),
        outcome: measurement.outcome,
        elapsed_ns: measurement.elapsed_ns,
        evidence: BTreeMap::from([
            (
                "bytes_written".to_owned(),
                measurement
                    .bytes_written
                    .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            ),
            ("trace_mode".to_owned(), "off".to_owned()),
        ]),
    }
}

fn future_schema_row(root: &Path) -> Result<CompatibilityRow, SuiteError> {
    fs::create_dir_all(root)?;
    let blob_root = root.join("blobs");
    let mut context = empty_context(11);
    context.schema_version = APPLICATION_CONTEXT_SCHEMA_VERSION + 1;
    let backend = ApplicationCheckpointBackend::new(
        BlobStore::open(blob_root).map_err(|error| SuiteError::Backend(error.to_string()))?,
    );
    let started = Instant::now();
    let outcome = match backend.capture(&context) {
        BackendOutcome::Unsupported(issue) => SampleOutcome::Unsupported {
            reason: issue.detail,
        },
        BackendOutcome::Failed(issue) => SampleOutcome::Failed {
            error: issue.detail,
        },
        BackendOutcome::Supported(_) => SampleOutcome::Failed {
            error: "future schema was unexpectedly accepted".to_owned(),
        },
    };
    Ok(CompatibilityRow {
        case: "application_future_schema".to_owned(),
        component: "application_checkpoint".to_owned(),
        configuration: BTreeMap::from([(
            "schema_version".to_owned(),
            context.schema_version.to_string(),
        )]),
        outcome,
        elapsed_ns: elapsed_ns(started),
        evidence: BTreeMap::from([("expected".to_owned(), "typed unsupported".to_owned())]),
    })
}

fn missing_reference_row(root: &Path) -> Result<CompatibilityRow, SuiteError> {
    fs::create_dir_all(root)?;
    let blob_root = root.join("blobs");
    let mut context = empty_context(12);
    context.messages.push(ObservableMessage {
        message_id: "missing-message".to_owned(),
        role: MessageRole::User,
        content_hash: missing_hash()?,
    });
    let backend = ApplicationCheckpointBackend::new(
        BlobStore::open(blob_root).map_err(|error| SuiteError::Backend(error.to_string()))?,
    );
    let started = Instant::now();
    let outcome = match backend.capture(&context) {
        BackendOutcome::Failed(issue) => SampleOutcome::Failed {
            error: issue.detail,
        },
        BackendOutcome::Unsupported(issue) => SampleOutcome::Unsupported {
            reason: issue.detail,
        },
        BackendOutcome::Supported(_) => SampleOutcome::Failed {
            error: "missing application reference was unexpectedly accepted".to_owned(),
        },
    };
    Ok(CompatibilityRow {
        case: "application_missing_reference".to_owned(),
        component: "application_checkpoint".to_owned(),
        configuration: BTreeMap::from([("reference".to_owned(), missing_hash()?.to_string())]),
        outcome,
        elapsed_ns: elapsed_ns(started),
        evidence: BTreeMap::from([("validation_stage".to_owned(), "capture".to_owned())]),
    })
}

fn missing_workspace_row(root: &Path) -> Result<CompatibilityRow, SuiteError> {
    fs::create_dir_all(root)?;
    let backend = WorkspaceBackend::open(root.join("blobs"), WorkspaceLimits::default())
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let started = Instant::now();
    let outcome = match backend.snapshot(root.join("absent")) {
        Ok(_) => SampleOutcome::Failed {
            error: "missing workspace was unexpectedly accepted".to_owned(),
        },
        Err(error) => SampleOutcome::Failed {
            error: bounded(&error.to_string()),
        },
    };
    Ok(CompatibilityRow {
        case: "workspace_missing_source".to_owned(),
        component: "workspace_checkpoint".to_owned(),
        configuration: BTreeMap::from([("source".to_owned(), "absent".to_owned())]),
        outcome,
        elapsed_ns: elapsed_ns(started),
        evidence: BTreeMap::from([("validation_stage".to_owned(), "capture".to_owned())]),
    })
}

fn special_workspace_row(root: &Path) -> Result<CompatibilityRow, SuiteError> {
    fs::create_dir_all(root)?;
    let backend = WorkspaceBackend::open(root.join("blobs"), WorkspaceLimits::default())
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    #[cfg(unix)]
    let source = tempfile::Builder::new()
        .prefix("epoch-sock-")
        .tempdir_in("/tmp")?;
    #[cfg(not(unix))]
    let source = tempfile::TempDir::new()?;
    #[cfg(unix)]
    let listener = std::os::unix::net::UnixListener::bind(source.path().join("socket"))?;
    let started = Instant::now();
    let outcome = match backend.snapshot(source.path()) {
        Err(WorkspaceError::Unsupported(reason)) => SampleOutcome::Unsupported {
            reason: format!("{reason:?}"),
        },
        Err(error) => SampleOutcome::Failed {
            error: bounded(&error.to_string()),
        },
        Ok(_) => SampleOutcome::Failed {
            error: "special workspace file was unexpectedly accepted".to_owned(),
        },
    };
    #[cfg(unix)]
    drop(listener);
    Ok(CompatibilityRow {
        case: "workspace_special_file".to_owned(),
        component: "workspace_checkpoint".to_owned(),
        configuration: BTreeMap::from([("entry".to_owned(), "unix_socket".to_owned())]),
        outcome,
        elapsed_ns: elapsed_ns(started),
        evidence: BTreeMap::from([(
            "policy".to_owned(),
            "special files require cooperation".to_owned(),
        )]),
    })
}

fn validation_cases(root: &Path, seed: u64) -> Result<Vec<ValidationCase>, SuiteError> {
    fs::create_dir_all(root)?;
    let future = future_schema_row(&root.join("future"))?;
    let missing = missing_reference_row(&root.join("missing"))?;
    let no_clobber = no_clobber_validation(&root.join("no-clobber"), seed)?;
    Ok(vec![
        ValidationCase {
            name: "future_application_schema_rejected".to_owned(),
            passed: matches!(future.outcome, SampleOutcome::Unsupported { .. }),
            detail: future.outcome.message().to_owned(),
        },
        ValidationCase {
            name: "missing_application_reference_rejected".to_owned(),
            passed: matches!(missing.outcome, SampleOutcome::Failed { .. }),
            detail: missing.outcome.message().to_owned(),
        },
        no_clobber,
    ])
}

fn no_clobber_validation(root: &Path, seed: u64) -> Result<ValidationCase, SuiteError> {
    let source = root.join("source");
    let target = root.join("target");
    fs::create_dir_all(&source)?;
    fs::create_dir_all(&target)?;
    fs::write(source.join("file"), seed.to_le_bytes())?;
    fs::write(target.join("sentinel"), b"preserve")?;
    let backend = WorkspaceBackend::open(root.join("blobs"), WorkspaceLimits::default())
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let snapshot = backend
        .snapshot(&source)
        .map_err(|error| SuiteError::Backend(error.to_string()))?;
    let rejected = matches!(
        backend.restore(&snapshot, &target),
        Err(WorkspaceError::TargetExists { .. })
    );
    let preserved = fs::read(target.join("sentinel"))? == b"preserve";
    Ok(ValidationCase {
        name: "workspace_restore_refuses_clobber".to_owned(),
        passed: rejected && preserved,
        detail: format!("target_rejected={rejected}, sentinel_preserved={preserved}"),
    })
}

fn application_context(
    blob_root: &Path,
    seed: u64,
    trace_mode: TraceMode,
) -> Result<ApplicationContext, SuiteError> {
    let store =
        BlobStore::open(blob_root).map_err(|error| SuiteError::Backend(error.to_string()))?;
    let records = if trace_mode == TraceMode::On {
        TRACE_RECORDS
    } else {
        0
    };
    let context_seed = seed & (i64::MAX as u64);
    let mut context = empty_context(context_seed);
    for index in 0..records {
        let bytes = format!("trace:{context_seed}:{index}").into_bytes();
        let blob = store
            .put(&bytes, "text/plain")
            .map_err(|error| SuiteError::Backend(error.to_string()))?;
        context.messages.push(ObservableMessage {
            message_id: format!("trace-{index}"),
            role: MessageRole::Assistant,
            content_hash: blob.hash,
        });
    }
    context.cursors.message_cursor = u64::from(records);
    Ok(context)
}

fn empty_context(seed: u64) -> ApplicationContext {
    ApplicationContext {
        schema_version: APPLICATION_CONTEXT_SCHEMA_VERSION,
        safe_point_id: format!("bench-{seed}"),
        deterministic_seed: seed,
        context_revision: 1,
        cursors: ResumeCursors {
            boundary_sequence: 1,
            message_cursor: 0,
            tool_cursor: 0,
            task_cursor: 0,
        },
        model_identifier: "recorded-benchmark-model".to_owned(),
        tool_registry: BTreeMap::new(),
        messages: Vec::new(),
        pending_tasks: Vec::new(),
        pending_model_request_ids: Vec::new(),
        pending_tool_call_ids: Vec::new(),
        user_visible_summary_hash: None,
    }
}

fn write_workspace(root: &Path, total_bytes: u64, files: u32, seed: u64) -> Result<(), SuiteError> {
    let quotient = total_bytes / u64::from(files);
    let remainder = total_bytes % u64::from(files);
    for index in 0..files {
        let length = quotient + u64::from(u64::from(index) < remainder);
        let length = usize::try_from(length)
            .map_err(|_| SuiteError::InvalidConfig("fixture file exceeds usize".to_owned()))?;
        let byte = seed.wrapping_add(u64::from(index)).to_le_bytes()[0];
        fs::write(
            root.join(format!("file-{index:04}.bin")),
            vec![byte; length],
        )?;
    }
    Ok(())
}

fn supported_checkpoint(
    outcome: BackendOutcome<epoch_checkpoint::ApplicationCheckpoint>,
) -> Result<epoch_checkpoint::ApplicationCheckpoint, SuiteError> {
    match outcome {
        BackendOutcome::Supported(value) => Ok(value),
        BackendOutcome::Unsupported(issue) => Err(SuiteError::Backend(format!(
            "application checkpoint unsupported: {}",
            issue.detail
        ))),
        BackendOutcome::Failed(issue) => Err(SuiteError::Backend(issue.detail)),
    }
}

fn supported_context(
    outcome: BackendOutcome<ApplicationContext>,
) -> Result<ApplicationContext, SuiteError> {
    match outcome {
        BackendOutcome::Supported(value) => Ok(value),
        BackendOutcome::Unsupported(issue) => Err(SuiteError::Backend(format!(
            "application restore unsupported: {}",
            issue.detail
        ))),
        BackendOutcome::Failed(issue) => Err(SuiteError::Backend(issue.detail)),
    }
}

fn missing_hash() -> Result<BlobHash, SuiteError> {
    BlobHash::from_str(&"0".repeat(64)).map_err(|error| SuiteError::Backend(error.to_string()))
}

fn validate_environment(environment: &BenchmarkEnvironment) -> Result<(), SuiteError> {
    environment
        .validate()
        .map_err(|error| SuiteError::InvalidConfig(error.to_string()))
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn metric(value: u64) -> f64 {
    value.to_string().parse().unwrap_or(f64::MAX)
}
