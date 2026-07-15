use std::{path::PathBuf, process::ExitCode};

use epoch_core::SessionId;
use epoch_supervisor::DirectSupervisor;
use epoch_trajectory::{
    ExportError, ExportLimits, TRAJECTORY_SCHEMA_VERSION, export_session, write_jsonl_new,
};
use serde::Serialize;

const TRUSTED_FAILURE_EXIT: u8 = 125;

pub(crate) struct ExportOptions {
    pub(crate) state_root: PathBuf,
    pub(crate) session: String,
    pub(crate) task_group: String,
    pub(crate) output: PathBuf,
    pub(crate) max_branches: usize,
    pub(crate) max_events_per_branch: usize,
}

#[derive(Serialize)]
struct ExportSummary<'a> {
    schema_version: u32,
    privacy_profile: &'static str,
    output: &'a std::path::Path,
    record_count: usize,
    labelled_count: usize,
    unlabelled_count: usize,
}

pub(crate) fn export(options: &ExportOptions) -> ExitCode {
    let Ok(session_id) = options.session.parse::<SessionId>() else {
        eprintln!("invalid session ID: {:?}", options.session);
        return ExitCode::from(2);
    };
    if let Err(error) = DirectSupervisor::open_existing(&options.state_root) {
        eprintln!("{error}");
        return if error.is_user_error() {
            ExitCode::from(2)
        } else {
            ExitCode::from(TRUSTED_FAILURE_EXIT)
        };
    }
    let records = match export_session(
        options.state_root.join("state.db"),
        session_id,
        &options.task_group,
        ExportLimits {
            max_branches: options.max_branches,
            max_events_per_branch: options.max_events_per_branch,
        },
    ) {
        Ok(records) => records,
        Err(error) => return report_export_error(&error),
    };
    if let Err(error) = write_jsonl_new(&options.output, &records) {
        return report_export_error(&error);
    }
    let labelled_count = records
        .iter()
        .filter(|record| record.success_label.is_some())
        .count();
    let summary = ExportSummary {
        schema_version: TRAJECTORY_SCHEMA_VERSION,
        privacy_profile: "metadata_only",
        output: &options.output,
        record_count: records.len(),
        labelled_count,
        unlabelled_count: records.len().saturating_sub(labelled_count),
    };
    match serde_json::to_string(&summary) {
        Ok(encoded) => {
            println!("{encoded}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("failed to encode ML export summary: {error}");
            ExitCode::from(TRUSTED_FAILURE_EXIT)
        }
    }
}

fn report_export_error(error: &ExportError) -> ExitCode {
    eprintln!("{error}");
    if error.is_user_error() {
        ExitCode::from(2)
    } else {
        ExitCode::from(TRUSTED_FAILURE_EXIT)
    }
}
