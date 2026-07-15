use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::ExitCode,
    str::FromStr as _,
};

use epoch_bench::{
    BenchmarkEnvironment, CheckpointSuiteConfig, CowConfig, DecisionThresholds, EvidenceBundle,
    SampleOutcome, SuiteName, SuiteRequest, run_suite,
};
use serde::Serialize;
use uuid::Uuid;

use crate::BenchFormat;

const INPUT_ERROR_EXIT: u8 = 2;
const BENCHMARK_FAILURE_EXIT: u8 = 125;
const MAX_REPORT_BYTES: u64 = 64 * 1024 * 1024;

pub(super) struct RunOptions {
    pub suite: String,
    pub root: PathBuf,
    pub warmups: u32,
    pub repetitions: u32,
    pub fixture_bytes: u64,
    pub fixture_files: u32,
    pub seed: u64,
    pub cow_allocation_bytes: u64,
    pub cow_children: u32,
    pub cow_dirty_basis_points: u32,
    pub cow_repetitions: u32,
}

#[derive(Serialize)]
struct RunSummary {
    schema_version: u32,
    run_id: String,
    suite: SuiteName,
    status: &'static str,
    artifact_root: String,
    report_json: String,
    samples_csv: String,
    results_markdown: String,
}

pub(super) fn run(options: &RunOptions) -> ExitCode {
    let suite = match SuiteName::from_str(&options.suite) {
        Ok(suite) => suite,
        Err(error) => return input_error(&error.to_string()),
    };
    let cow = match CowConfig::new(
        options.cow_allocation_bytes,
        options.cow_children,
        options.cow_dirty_basis_points,
        options.cow_repetitions,
    ) {
        Ok(cow) => cow,
        Err(error) => return input_error(&error.to_string()),
    };
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let environment = match BenchmarkEnvironment::collect(&repository) {
        Ok(environment) => environment,
        Err(error) => return benchmark_error(&error.to_string()),
    };
    let root = match safe_root(&options.root, false) {
        Ok(root) => root,
        Err(error) => return input_error(&error),
    };
    let scratch = match tempfile::Builder::new()
        .prefix(".epoch-bench-scratch-")
        .tempdir_in(&root)
    {
        Ok(scratch) => scratch,
        Err(error) => return benchmark_error(&error.to_string()),
    };
    let checkpoint = CheckpointSuiteConfig {
        root: scratch.path().to_path_buf(),
        seed: options.seed,
        warmups: options.warmups,
        repetitions: options.repetitions,
        fixture_bytes: options.fixture_bytes,
        fixture_files: options.fixture_files,
    };
    if let Err(error) = checkpoint.validate() {
        return input_error(&error.to_string());
    }
    let request = SuiteRequest {
        suite,
        checkpoint,
        cow,
        thresholds: DecisionThresholds::week4(),
    };
    let bundle = match run_suite(&request, environment) {
        Ok(bundle) => bundle,
        Err(error) => return benchmark_error(&error.to_string()),
    };
    let artifact_root = root.join(&bundle.run_id);
    if let Err(error) = persist(&root, &artifact_root, &bundle) {
        return benchmark_error(&error);
    }
    let summary = RunSummary {
        schema_version: 1,
        run_id: bundle.run_id.clone(),
        suite,
        status: completion_status(&bundle),
        report_json: artifact_root.join("report.json").display().to_string(),
        samples_csv: artifact_root.join("samples.csv").display().to_string(),
        results_markdown: artifact_root.join("RESULTS.md").display().to_string(),
        artifact_root: artifact_root.display().to_string(),
    };
    match serde_json::to_string_pretty(&summary) {
        Ok(encoded) => {
            println!("{encoded}");
            ExitCode::SUCCESS
        }
        Err(error) => benchmark_error(&error.to_string()),
    }
}

pub(super) fn report(run_id: &str, root: &Path, format: BenchFormat) -> ExitCode {
    if !valid_run_id(run_id) {
        return input_error("benchmark run ID must be bench- followed by a canonical UUID");
    }
    let root = match safe_root(root, true) {
        Ok(root) => root,
        Err(error) => return input_error(&error),
    };
    let run_root = root.join(run_id);
    if let Err(error) = validate_existing_directory(&run_root) {
        return input_error(&error);
    }
    let report_path = run_root.join("report.json");
    let encoded = match bounded_read(&report_path) {
        Ok(encoded) => encoded,
        Err(error) => return benchmark_error(&error),
    };
    match serde_json::from_slice::<EvidenceBundle>(&encoded) {
        Ok(bundle) if bundle.run_id == run_id => {}
        Ok(_) => return benchmark_error("report run ID does not match its directory"),
        Err(error) => return benchmark_error(&format!("invalid benchmark report: {error}")),
    }
    let artifact = match format {
        BenchFormat::Json => encoded,
        BenchFormat::Csv => match bounded_read(&run_root.join("samples.csv")) {
            Ok(encoded) => encoded,
            Err(error) => return benchmark_error(&error),
        },
        BenchFormat::Markdown => match bounded_read(&run_root.join("RESULTS.md")) {
            Ok(encoded) => encoded,
            Err(error) => return benchmark_error(&error),
        },
    };
    if let Err(error) = String::from_utf8(artifact).map(|output| print!("{output}")) {
        return benchmark_error(&format!("benchmark artifact is not UTF-8: {error}"));
    }
    ExitCode::SUCCESS
}

fn safe_root(path: &Path, must_exist: bool) -> Result<PathBuf, String> {
    if path.components().any(|part| part == Component::ParentDir) {
        return Err("benchmark root must not contain parent-directory components".to_owned());
    }
    let root = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    if root.parent().is_none() {
        return Err("benchmark root must not be the filesystem root".to_owned());
    }
    if root.exists() {
        validate_existing_directory(&root)?;
    } else if must_exist {
        return Err("benchmark root does not exist".to_owned());
    } else {
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    }
    Ok(root)
}

fn validate_existing_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        Err("benchmark path must be a real directory, not a file or symlink".to_owned())
    } else {
        Ok(())
    }
}

fn persist(root: &Path, artifact_root: &Path, bundle: &EvidenceBundle) -> Result<(), String> {
    if artifact_root.exists() {
        return Err("benchmark run directory already exists".to_owned());
    }
    let staging = root.join(format!(".epoch-bench-stage-{}", Uuid::new_v4()));
    fs::create_dir(&staging).map_err(|error| error.to_string())?;
    let result = (|| {
        fs::write(
            staging.join("report.json"),
            bundle.to_json().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            staging.join("samples.csv"),
            bundle.to_csv().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        fs::write(staging.join("RESULTS.md"), bundle.to_markdown())
            .map_err(|error| error.to_string())?;
        fs::rename(&staging, artifact_root).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn bounded_read(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("benchmark artifact must be a regular non-symlink file".to_owned());
    }
    if metadata.len() > MAX_REPORT_BYTES {
        return Err(format!(
            "benchmark artifact exceeds {MAX_REPORT_BYTES} bytes"
        ));
    }
    fs::read(path).map_err(|error| error.to_string())
}

fn completion_status(bundle: &EvidenceBundle) -> &'static str {
    let checkpoint_failed = bundle.checkpoint.as_ref().is_some_and(|checkpoint| {
        checkpoint
            .reports
            .iter()
            .any(|report| report.summary.failed > 0)
            || checkpoint.validation_cases.iter().any(|case| !case.passed)
    });
    let cow_failed = bundle.cow.as_ref().is_some_and(|matrix| {
        matrix
            .points
            .iter()
            .any(|point| matches!(point.outcome, SampleOutcome::Failed { .. }))
    });
    let fault_failed = bundle.faults.as_ref().is_some_and(|matrix| {
        matrix.rows.iter().any(|row| {
            row.evidence_kind == epoch_bench::EvidenceKind::Actual
                && (!row.containment_verified
                    || matches!(row.outcome, SampleOutcome::Failed { .. }))
        })
    });
    if checkpoint_failed || cow_failed || fault_failed {
        "completed_with_failures"
    } else if bundle.cow.as_ref().is_some_and(|matrix| {
        matrix
            .points
            .iter()
            .any(|point| matches!(point.outcome, SampleOutcome::Unsupported { .. }))
    }) || bundle.compatibility.as_ref().is_some_and(|matrix| {
        matrix
            .rows
            .iter()
            .any(|row| matches!(row.outcome, SampleOutcome::Unsupported { .. }))
    }) {
        "completed_with_unsupported"
    } else {
        "completed"
    }
}

fn valid_run_id(value: &str) -> bool {
    value
        .strip_prefix("bench-")
        .and_then(|value| Uuid::parse_str(value).ok())
        .is_some_and(|uuid| format!("bench-{uuid}") == value)
}

fn input_error(message: &str) -> ExitCode {
    eprintln!("benchmark input rejected: {message}");
    ExitCode::from(INPUT_ERROR_EXIT)
}

fn benchmark_error(message: &str) -> ExitCode {
    eprintln!("benchmark failed: {message}");
    ExitCode::from(BENCHMARK_FAILURE_EXIT)
}
