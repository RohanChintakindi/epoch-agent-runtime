use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::Value;

const MAX_REPORTS: usize = 32;
const MAX_REPORT_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct BenchmarkReader {
    root: PathBuf,
}

impl BenchmarkReader {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn read(&self) -> BenchmarkView {
        let Ok(metadata) = fs::symlink_metadata(&self.root) else {
            return BenchmarkView::unavailable("results_directory_missing");
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return BenchmarkView::unavailable("results_path_not_directory");
        }
        let Ok(entries) = fs::read_dir(&self.root) else {
            return BenchmarkView::unavailable("results_directory_unreadable");
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>();
        paths.sort();
        let mut reports = Vec::new();
        let mut skipped = 0_usize;
        for path in paths {
            if reports.len() == MAX_REPORTS {
                skipped = skipped.saturating_add(1);
                continue;
            }
            match read_card(&path) {
                Some(card) => reports.push(card),
                None => skipped = skipped.saturating_add(1),
            }
        }
        BenchmarkView {
            available: true,
            reason: None,
            reports,
            skipped,
            limits: BenchmarkLimits {
                maximum_reports: MAX_REPORTS,
                maximum_report_bytes: MAX_REPORT_BYTES,
            },
        }
    }
}

fn read_card(path: &Path) -> Option<BenchmarkCard> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_REPORT_BYTES
    {
        return None;
    }
    let encoded = fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&encoded).ok()?;
    let schema_version = value.get("schema_version")?.as_u64()?;
    let config = value.get("config")?;
    let summary = value.get("summary")?;
    let latency = summary.get("latency_ns")?;
    Some(BenchmarkCard {
        file: bounded_file_name(path)?,
        schema_version,
        suite: bounded_string(config.get("suite")?, 128)?,
        backend: bounded_string(config.get("backend")?, 128)?,
        trace_mode: bounded_string(config.get("trace_mode")?, 32)?,
        repetitions: config.get("repetitions")?.as_u64()?,
        succeeded: summary.get("succeeded")?.as_u64()?,
        unsupported: summary.get("unsupported")?.as_u64()?,
        failed: summary.get("failed")?.as_u64()?,
        p50_ns: optional_u64(latency.get("p50")),
        p95_ns: optional_u64(latency.get("p95")),
        p99_ns: optional_u64(latency.get("p99")),
    })
}

fn optional_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64)
}

fn bounded_string(value: &Value, maximum: usize) -> Option<String> {
    let value = value.as_str()?;
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        None
    } else {
        Some(value.to_owned())
    }
}

fn bounded_file_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    if name.is_empty() || name.len() > 160 || name.chars().any(char::is_control) {
        None
    } else {
        Some(name.to_owned())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkView {
    pub available: bool,
    pub reason: Option<&'static str>,
    pub reports: Vec<BenchmarkCard>,
    pub skipped: usize,
    pub limits: BenchmarkLimits,
}

impl BenchmarkView {
    fn unavailable(reason: &'static str) -> Self {
        Self {
            available: false,
            reason: Some(reason),
            reports: Vec::new(),
            skipped: 0,
            limits: BenchmarkLimits {
                maximum_reports: MAX_REPORTS,
                maximum_report_bytes: MAX_REPORT_BYTES,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkLimits {
    pub maximum_reports: usize,
    pub maximum_report_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchmarkCard {
    pub file: String,
    pub schema_version: u64,
    pub suite: String,
    pub backend: String,
    pub trace_mode: String,
    pub repetitions: u64,
    pub succeeded: u64,
    pub unsupported: u64,
    pub failed: u64,
    pub p50_ns: Option<u64>,
    pub p95_ns: Option<u64>,
    pub p99_ns: Option<u64>,
}
