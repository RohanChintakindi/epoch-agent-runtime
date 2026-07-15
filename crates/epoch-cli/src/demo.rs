use std::{
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode},
    time::Instant,
};

use epoch_core::SessionId;
use serde::Serialize;
use serde_json::{Value, json};

const DEMO_FAILURE_EXIT: u8 = 125;
const DEMO_INPUT_EXIT: u8 = 2;
const DIAGNOSTIC_LIMIT: usize = 4_096;
const MARKER_NAME: &str = ".epoch-demo-owned.json";
const MARKER_CONTENT: &str = "{\"owner\":\"epoch-demo\",\"schema_version\":1}\n";
const BASELINE_SEED: u64 = 424_242;
const VARIANT_SEED: u64 = 424_243;

pub struct DemoConfig {
    pub agent: PathBuf,
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub json: bool,
}

#[derive(Serialize)]
struct DemoReport {
    schema_version: u16,
    code_revision: String,
    code_dirty: bool,
    outcome: &'static str,
    run_id: Option<String>,
    run_root: Option<String>,
    workspace_root: Option<String>,
    report_path: Option<String>,
    phases: Vec<PhaseReport>,
    unsupported_sections: Vec<UnsupportedSection>,
    summary: String,
    failure: Option<DemoFailure>,
}

#[derive(Serialize)]
struct PhaseReport {
    name: &'static str,
    status: &'static str,
    duration_ms: u64,
    evidence: Value,
    diagnostic: Option<String>,
}

#[derive(Clone, Serialize)]
struct DemoFailure {
    code: &'static str,
    diagnostic: String,
}

#[derive(Serialize)]
struct UnsupportedSection {
    section: &'static str,
    outcome: &'static str,
    code: &'static str,
    detail: &'static str,
}

#[derive(Serialize)]
struct DemoManifest {
    schema_version: u16,
    name: String,
    executable: String,
    arguments: Vec<String>,
    working_directory: String,
}

struct DemoPaths {
    run_id: String,
    run_root: PathBuf,
    workspace_root: PathBuf,
    baseline_workspace: PathBuf,
    restored_workspace: PathBuf,
    report_path: PathBuf,
    baseline_manifest: PathBuf,
    variant_manifest: PathBuf,
    executable: PathBuf,
}

struct DemoRunner {
    paths: DemoPaths,
    phases: Vec<PhaseReport>,
    baseline_artifact: Option<Vec<u8>>,
}

struct BaselineIds {
    session_id: String,
    epoch_id: String,
}

struct VariantIds {
    epoch_id: String,
}

struct CommandFailure {
    diagnostic: String,
    evidence: Value,
}

pub fn run(config: &DemoConfig) -> ExitCode {
    let json_output = config.json;
    let paths = match prepare(config) {
        Ok(paths) => paths,
        Err(failure) => {
            let report = failed_report(None, Vec::new(), failure);
            emit(&report, json_output);
            return ExitCode::from(DEMO_INPUT_EXIT);
        }
    };
    let mut runner = DemoRunner {
        paths,
        phases: Vec::new(),
        baseline_artifact: None,
    };
    let result = runner.execute();
    let (outcome, summary, failure, exit) = match result {
        Ok(()) => (
            "completed_with_unsupported",
            format!(
                "Epoch demo completed: {0}/{0} real phases succeeded; four narrow boundaries remain explicitly unsupported.",
                runner.phases.len()
            ),
            None,
            ExitCode::SUCCESS,
        ),
        Err(failure) => (
            "failed",
            format!(
                "Epoch demo stopped after {} recorded phases; evidence was preserved.",
                runner.phases.len()
            ),
            Some(failure),
            ExitCode::from(DEMO_FAILURE_EXIT),
        ),
    };
    let (code_revision, code_dirty) = collect_code_revision();
    let report = DemoReport {
        schema_version: 1,
        code_revision,
        code_dirty,
        outcome,
        run_id: Some(runner.paths.run_id.clone()),
        run_root: Some(display(&runner.paths.run_root)),
        workspace_root: Some(display(&runner.paths.workspace_root)),
        report_path: Some(display(&runner.paths.report_path)),
        phases: runner.phases,
        unsupported_sections: unsupported_sections(),
        summary,
        failure,
    };
    if let Err(error) = persist_report(&runner.paths.report_path, &report) {
        eprintln!("demo report persistence failed: {error}");
        return ExitCode::from(DEMO_FAILURE_EXIT);
    }
    emit(&report, json_output);
    exit
}

impl DemoRunner {
    fn execute(&mut self) -> Result<(), DemoFailure> {
        self.doctor()?;
        let baseline = self.baseline()?;
        self.change_workspace()?;
        let variant = self.variant()?;
        self.restore_diff_and_fork(&baseline, &variant)
    }

    fn doctor(&mut self) -> Result<(), DemoFailure> {
        self.command_phase("doctor", vec!["doctor".into(), "--json".into()], |value| {
            Ok(json!({
                "os": required(value, "/os")?,
                "architecture": required(value, "/architecture")?,
                "control_plane": required(value, "/control_plane")?,
                "backends": required(value, "/backends")?,
            }))
        })?;
        Ok(())
    }

    fn baseline(&mut self) -> Result<BaselineIds, DemoFailure> {
        let run = self.command_phase(
            "run_baseline",
            vec![
                "run".into(),
                "--manifest".into(),
                self.paths.baseline_manifest.as_os_str().to_owned(),
            ],
            run_evidence,
        )?;
        let session_id = required_string(&run, "/session_id")?;
        let branch_id = required_string(&run, "/branch_id")?;
        self.command_phase(
            "status_before_checkpoint",
            vec!["status".into(), session_id.clone().into()],
            |value| {
                Ok(json!({
                    "session_id": required(value, "/session_id")?,
                    "state": required(value, "/state")?,
                    "branch_count": required_array(value, "/branches")?.len(),
                }))
            },
        )?;
        self.command_phase(
            "events",
            vec![
                "events".into(),
                session_id.clone().into(),
                "--branch".into(),
                branch_id.clone().into(),
                "--limit".into(),
                "1000".into(),
            ],
            events_evidence,
        )?;
        let checkpoint = self.command_phase(
            "checkpoint_baseline",
            vec![
                "checkpoint".into(),
                session_id.clone().into(),
                "--branch".into(),
                branch_id.clone().into(),
                "--label".into(),
                "demo-baseline".into(),
            ],
            checkpoint_evidence,
        )?;
        self.baseline_artifact = Some(
            fs::read(self.paths.baseline_workspace.join("artifact.txt")).map_err(|error| {
                self.local_failure(
                    "checkpoint_baseline_artifact",
                    Instant::now(),
                    "checkpoint_artifact_unreadable",
                    error.to_string(),
                )
            })?,
        );
        Ok(BaselineIds {
            session_id,
            epoch_id: required_string(&checkpoint, "/result/epoch_id")?,
        })
    }

    fn change_workspace(&mut self) -> Result<(), DemoFailure> {
        let started = Instant::now();
        let artifact = self.paths.baseline_workspace.join("artifact.txt");
        match fs::write(&artifact, b"changed-after-application-checkpoint") {
            Ok(()) => {
                self.phases.push(PhaseReport {
                    name: "controlled_workspace_change",
                    status: "succeeded",
                    duration_ms: elapsed_ms(started),
                    evidence: json!({
                        "artifact": display(&artifact),
                        "changed": true,
                        "workspace_checkpoint": "captured_before_change",
                    }),
                    diagnostic: None,
                });
                Ok(())
            }
            Err(error) => Err(self.local_failure(
                "controlled_workspace_change",
                started,
                "workspace_change_failed",
                error.to_string(),
            )),
        }
    }

    fn variant(&mut self) -> Result<VariantIds, DemoFailure> {
        let run = self.command_phase(
            "run_variant",
            vec![
                "run".into(),
                "--manifest".into(),
                self.paths.variant_manifest.as_os_str().to_owned(),
            ],
            run_evidence,
        )?;
        let session = required_string(&run, "/session_id")?;
        let branch = required_string(&run, "/branch_id")?;
        let checkpoint = self.command_phase(
            "checkpoint_variant",
            vec![
                "checkpoint".into(),
                session.into(),
                "--branch".into(),
                branch.into(),
                "--label".into(),
                "demo-variant".into(),
            ],
            checkpoint_evidence,
        )?;
        Ok(VariantIds {
            epoch_id: required_string(&checkpoint, "/result/epoch_id")?,
        })
    }

    fn restore_diff_and_fork(
        &mut self,
        baseline: &BaselineIds,
        variant: &VariantIds,
    ) -> Result<(), DemoFailure> {
        let restored = self.command_phase(
            "restore_baseline",
            vec![
                "restore".into(),
                baseline.epoch_id.clone().into(),
                "--workspace-target".into(),
                self.paths.restored_workspace.as_os_str().to_owned(),
            ],
            |value| {
                Ok(json!({
                    "epoch_id": required(value, "/result/epoch_id")?,
                    "activated": required(value, "/result/activated")?,
                    "process_restored": required(value, "/result/process_restored")?,
                    "workspace_restored": required(value, "/result/workspace_restored")?,
                    "workspace_target": required(value, "/result/workspace_target")?,
                }))
            },
        )?;
        let _ = required_string(&restored, "/result/epoch_id")?;
        self.status_after_restore(baseline)?;
        self.semantic_diff(baseline, variant)?;
        let fork = self.command_phase(
            "fork",
            vec![
                "fork".into(),
                baseline.epoch_id.clone().into(),
                "--name".into(),
                "interview-branch".into(),
            ],
            fork_evidence,
        )?;
        let child = required_string(&fork, "/result/branch_id")?;
        self.command_phase(
            "branch_inspect",
            vec!["branch".into(), "inspect".into(), child.into()],
            fork_evidence,
        )?;
        Ok(())
    }

    fn status_after_restore(&mut self, baseline: &BaselineIds) -> Result<(), DemoFailure> {
        let artifact = self.paths.baseline_workspace.join("artifact.txt");
        let original_workspace_change_preserved =
            fs::read(&artifact).is_ok_and(|bytes| bytes == b"changed-after-application-checkpoint");
        let restored_workspace_matches_checkpoint =
            self.baseline_artifact.as_deref().is_some_and(|expected| {
                fs::read(self.paths.restored_workspace.join("artifact.txt"))
                    .is_ok_and(|bytes| bytes == expected)
            });
        self.command_phase(
            "status_after_restore",
            vec!["status".into(), baseline.session_id.clone().into()],
            |value| {
                Ok(json!({
                    "session_id": required(value, "/session_id")?,
                    "state": required(value, "/state")?,
                    "current_epoch_id": required(value, "/application/result/current_epoch_id")?,
                    "original_workspace_change_preserved": original_workspace_change_preserved,
                    "restored_workspace_matches_checkpoint": restored_workspace_matches_checkpoint,
                }))
            },
        )?;
        Ok(())
    }

    fn semantic_diff(
        &mut self,
        baseline: &BaselineIds,
        variant: &VariantIds,
    ) -> Result<(), DemoFailure> {
        self.command_phase(
            "semantic_diff",
            vec![
                "diff".into(),
                baseline.epoch_id.clone().into(),
                variant.epoch_id.clone().into(),
                "--json".into(),
            ],
            |value| {
                Ok(json!({
                    "before_epoch_id": required(value, "/result/before_epoch_id")?,
                    "after_epoch_id": required(value, "/result/after_epoch_id")?,
                    "identical": required(value, "/result/diff/identical")?,
                    "change_count": required_array(value, "/result/diff/changes")?.len(),
                    "workspace": required(value, "/result/workspace")?,
                    "capabilities": required(value, "/result/capabilities")?,
                    "effects": required(value, "/result/effects")?,
                }))
            },
        )?;
        Ok(())
    }

    fn command_phase(
        &mut self,
        name: &'static str,
        arguments: Vec<OsString>,
        evidence: impl FnOnce(&Value) -> Result<Value, DemoFailure>,
    ) -> Result<Value, DemoFailure> {
        let started = Instant::now();
        match invoke_json(&self.paths.executable, &self.paths.run_root, arguments) {
            Ok(value) => match evidence(&value) {
                Ok(evidence) => {
                    self.phases.push(PhaseReport {
                        name,
                        status: "succeeded",
                        duration_ms: elapsed_ms(started),
                        evidence,
                        diagnostic: None,
                    });
                    Ok(value)
                }
                Err(failure) => Err(self.phase_failure(name, started, failure, json!({}))),
            },
            Err(failure) => Err(self.phase_failure(
                name,
                started,
                DemoFailure {
                    code: "command_failed",
                    diagnostic: failure.diagnostic,
                },
                failure.evidence,
            )),
        }
    }

    fn phase_failure(
        &mut self,
        name: &'static str,
        started: Instant,
        failure: DemoFailure,
        evidence: Value,
    ) -> DemoFailure {
        self.phases.push(PhaseReport {
            name,
            status: "failed",
            duration_ms: elapsed_ms(started),
            evidence,
            diagnostic: Some(bounded(&failure.diagnostic)),
        });
        failure
    }

    fn local_failure(
        &mut self,
        name: &'static str,
        started: Instant,
        code: &'static str,
        diagnostic: String,
    ) -> DemoFailure {
        self.phase_failure(name, started, DemoFailure { code, diagnostic }, json!({}))
    }
}

fn prepare(config: &DemoConfig) -> Result<DemoPaths, DemoFailure> {
    validate_paths(&config.root, &config.workspace)?;
    let agent = validate_agent(&config.agent)?;
    claim_root(&config.root)?;
    claim_workspace(&config.workspace)?;
    let run_id = format!("run-{}", SessionId::new());
    let run_root = config.root.join("runs").join(&run_id);
    let workspace_root = config.workspace.join(&run_id);
    let baseline_workspace = workspace_root.join("baseline");
    let variant_workspace = workspace_root.join("variant");
    let restored_workspace = workspace_root.join("restored-baseline");
    fs::create_dir_all(&run_root).map_err(|error| io_failure(&error))?;
    fs::create_dir_all(&baseline_workspace).map_err(|error| io_failure(&error))?;
    fs::create_dir_all(&variant_workspace).map_err(|error| io_failure(&error))?;
    let baseline_manifest = run_root.join("baseline.toml");
    let variant_manifest = run_root.join("variant.toml");
    write_manifest(
        &baseline_manifest,
        &agent,
        BASELINE_SEED,
        &baseline_workspace,
    )?;
    write_manifest(&variant_manifest, &agent, VARIANT_SEED, &variant_workspace)?;
    Ok(DemoPaths {
        run_id,
        report_path: run_root.join("report.json"),
        run_root,
        workspace_root,
        baseline_workspace,
        restored_workspace,
        baseline_manifest,
        variant_manifest,
        executable: std::env::current_exe().map_err(|error| io_failure(&error))?,
    })
}

fn validate_paths(root: &Path, workspace: &Path) -> Result<(), DemoFailure> {
    if !root.is_absolute() || !workspace.is_absolute() {
        return Err(input_failure(
            "unsafe_demo_root",
            "demo root and workspace must be absolute paths",
        ));
    }
    if has_parent_component(root) || has_parent_component(workspace) {
        return Err(input_failure(
            "unsafe_demo_root",
            "demo paths must not contain parent-directory components",
        ));
    }
    if workspace == root || !workspace.starts_with(root) {
        return Err(input_failure(
            "unsafe_demo_root",
            "workspace must be a distinct path inside the dedicated demo root",
        ));
    }
    Ok(())
}

fn validate_agent(agent: &Path) -> Result<PathBuf, DemoFailure> {
    if !agent.is_absolute() || !agent.is_file() {
        return Err(input_failure(
            "invalid_test_agent",
            "test-agent path must be an absolute executable file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(agent)
            .map_err(|error| io_failure(&error))?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(input_failure(
                "invalid_test_agent",
                "test-agent file is not executable",
            ));
        }
    }
    fs::canonicalize(agent).map_err(|error| io_failure(&error))
}

fn claim_root(root: &Path) -> Result<(), DemoFailure> {
    if root.exists() {
        let metadata = fs::symlink_metadata(root).map_err(|error| io_failure(&error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(input_failure(
                "unsafe_demo_root",
                "demo root must be a real directory, not a file or symlink",
            ));
        }
        let mut entries = fs::read_dir(root).map_err(|error| io_failure(&error))?;
        if entries
            .next()
            .transpose()
            .map_err(|error| io_failure(&error))?
            .is_some()
        {
            let marker = fs::read_to_string(root.join(MARKER_NAME)).map_err(|_| {
                input_failure(
                    "unsafe_demo_root",
                    "nonempty demo root is missing the exact Epoch ownership marker",
                )
            })?;
            if marker != MARKER_CONTENT {
                return Err(input_failure(
                    "unsafe_demo_root",
                    "demo ownership marker is invalid",
                ));
            }
            return Ok(());
        }
    } else {
        fs::create_dir_all(root).map_err(|error| io_failure(&error))?;
    }
    fs::write(root.join(MARKER_NAME), MARKER_CONTENT).map_err(|error| io_failure(&error))
}

fn claim_workspace(workspace: &Path) -> Result<(), DemoFailure> {
    if workspace.exists() {
        let metadata = fs::symlink_metadata(workspace).map_err(|error| io_failure(&error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(input_failure(
                "unsafe_demo_root",
                "demo workspace must be a real directory, not a file or symlink",
            ));
        }
        Ok(())
    } else {
        fs::create_dir_all(workspace).map_err(|error| io_failure(&error))
    }
}

fn write_manifest(
    path: &Path,
    agent: &Path,
    seed: u64,
    workspace: &Path,
) -> Result<(), DemoFailure> {
    let manifest = DemoManifest {
        schema_version: 1,
        name: format!("epoch-demo-{seed}"),
        executable: display(agent),
        arguments: vec![
            "--seed".to_owned(),
            seed.to_string(),
            "--scenario".to_owned(),
            "files".to_owned(),
            "--workspace".to_owned(),
            display(workspace),
        ],
        working_directory: display(workspace),
    };
    let encoded = toml::to_string(&manifest).map_err(|error| DemoFailure {
        code: "manifest_encoding_failed",
        diagnostic: error.to_string(),
    })?;
    fs::write(path, encoded).map_err(|error| io_failure(&error))
}

fn invoke_json(
    executable: &Path,
    run_root: &Path,
    arguments: Vec<OsString>,
) -> Result<Value, CommandFailure> {
    let output = Command::new(executable)
        .current_dir(run_root)
        .args(arguments)
        .output()
        .map_err(|error| CommandFailure {
            diagnostic: error.to_string(),
            evidence: json!({}),
        })?;
    let decoded = serde_json::from_slice::<Value>(&output.stdout);
    if !output.status.success() {
        let child = decoded.unwrap_or_else(
            |_| json!({"stdout": bounded(&String::from_utf8_lossy(&output.stdout))}),
        );
        return Err(CommandFailure {
            diagnostic: bounded(&format!(
                "child exit {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            )),
            evidence: json!({"exit_code": output.status.code(), "child": child}),
        });
    }
    decoded.map_err(|error| CommandFailure {
        diagnostic: format!("successful child returned invalid JSON: {error}"),
        evidence: json!({"stdout": bounded(&String::from_utf8_lossy(&output.stdout))}),
    })
}

fn run_evidence(value: &Value) -> Result<Value, DemoFailure> {
    Ok(json!({
        "session_id": required(value, "/session_id")?,
        "branch_id": required(value, "/branch_id")?,
        "termination": required(value, "/termination")?,
        "protocol_records": required(value, "/protocol_records")?,
    }))
}

fn events_evidence(value: &Value) -> Result<Value, DemoFailure> {
    let events = required_array(value, "/events")?;
    Ok(json!({
        "session_id": required(value, "/session_id")?,
        "branch_id": required(value, "/branch_id")?,
        "event_count": events.len(),
        "first_event_id": events.first().and_then(|event| event.get("event_id")),
        "last_event_id": events.last().and_then(|event| event.get("event_id")),
    }))
}

fn checkpoint_evidence(value: &Value) -> Result<Value, DemoFailure> {
    Ok(json!({
        "epoch_id": required(value, "/result/epoch_id")?,
        "session_id": required(value, "/result/session_id")?,
        "branch_id": required(value, "/result/branch_id")?,
        "component_hash": required(value, "/result/component_hash")?,
        "boundary_sequence": required(value, "/result/boundary_sequence")?,
        "restore_scope": required(value, "/result/restore_scope")?,
    }))
}

fn fork_evidence(value: &Value) -> Result<Value, DemoFailure> {
    Ok(json!({
        "session_id": required(value, "/result/session_id")?,
        "branch_id": required(value, "/result/branch_id")?,
        "parent_branch_id": required(value, "/result/parent_branch_id")?,
        "fork_epoch_id": required(value, "/result/fork_epoch_id")?,
        "fork_point_sequence": required(value, "/result/fork_point_sequence")?,
        "replay_continuation": required(value, "/result/replay/continuation/outcome")?,
        "effect_frontier": required(value, "/result/effect_frontier/outcome")?,
    }))
}

fn required<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value, DemoFailure> {
    value.pointer(pointer).ok_or_else(|| DemoFailure {
        code: "invalid_phase_evidence",
        diagnostic: format!("child JSON is missing {pointer}"),
    })
}

fn required_string(value: &Value, pointer: &str) -> Result<String, DemoFailure> {
    required(value, pointer)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| DemoFailure {
            code: "invalid_phase_evidence",
            diagnostic: format!("child JSON field {pointer} is not a string"),
        })
}

fn required_array<'a>(value: &'a Value, pointer: &str) -> Result<&'a Vec<Value>, DemoFailure> {
    required(value, pointer)?
        .as_array()
        .ok_or_else(|| DemoFailure {
            code: "invalid_phase_evidence",
            diagnostic: format!("child JSON field {pointer} is not an array"),
        })
}

fn unsupported_sections() -> Vec<UnsupportedSection> {
    vec![
        UnsupportedSection {
            section: "continuation",
            outcome: "unsupported",
            code: "autonomous_branch_continuation_pending",
            detail: "fork lineage is durable but autonomous execution continuation is not registered",
        },
        UnsupportedSection {
            section: "effects",
            outcome: "unsupported",
            code: "external_effect_delivery_reconciliation_pending",
            detail: "effect frontiers are durable but live provider delivery reconciliation is not registered",
        },
        UnsupportedSection {
            section: "isolation",
            outcome: "unsupported",
            code: "linux_supervisor_adapter_pending",
            detail: "the native-tested Linux backend is not composed into the supervisor launch path",
        },
        UnsupportedSection {
            section: "process",
            outcome: "unsupported",
            code: "process_checkpoint_backend_not_registered",
            detail: "application and workspace state restore without live process memory",
        },
    ]
}

fn collect_code_revision() -> (String, bool) {
    let revision = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| value.len() == 40)
        .unwrap_or_else(|| "unavailable".to_owned());
    let dirty = revision == "unavailable"
        || Command::new("git")
            .args(["status", "--porcelain", "--untracked-files=no"])
            .output()
            .map_or(true, |output| {
                !output.status.success() || !output.stdout.is_empty()
            });
    (revision, dirty)
}

fn failed_report(
    paths: Option<&DemoPaths>,
    phases: Vec<PhaseReport>,
    failure: DemoFailure,
) -> DemoReport {
    let (code_revision, code_dirty) = collect_code_revision();
    DemoReport {
        schema_version: 1,
        code_revision,
        code_dirty,
        outcome: "failed",
        run_id: paths.map(|paths| paths.run_id.clone()),
        run_root: paths.map(|paths| display(&paths.run_root)),
        workspace_root: paths.map(|paths| display(&paths.workspace_root)),
        report_path: paths.map(|paths| display(&paths.report_path)),
        phases,
        unsupported_sections: unsupported_sections(),
        summary: "Epoch demo did not start because its safety contract was not satisfied."
            .to_owned(),
        failure: Some(failure),
    }
}

fn persist_report(path: &Path, report: &DemoReport) -> Result<(), std::io::Error> {
    let encoded = serde_json::to_vec_pretty(report).map_err(std::io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, encoded)?;
    fs::rename(temporary, path)
}

fn emit(report: &DemoReport, json_output: bool) {
    if json_output {
        match serde_json::to_string_pretty(report) {
            Ok(encoded) => println!("{encoded}"),
            Err(error) => eprintln!("demo report serialization failed: {error}"),
        }
        return;
    }
    println!("{}", report.summary);
    for phase in &report.phases {
        println!(
            "  {:<30} {:<9} {} ms",
            phase.name, phase.status, phase.duration_ms
        );
    }
    if let Some(path) = &report.report_path {
        println!("  evidence: {path}");
    }
    if let Some(failure) = &report.failure {
        eprintln!("  failure [{}]: {}", failure.code, failure.diagnostic);
    }
}

fn input_failure(code: &'static str, diagnostic: &str) -> DemoFailure {
    DemoFailure {
        code,
        diagnostic: diagnostic.to_owned(),
    }
}

fn io_failure(error: &std::io::Error) -> DemoFailure {
    DemoFailure {
        code: "demo_io_failure",
        diagnostic: error.to_string(),
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn bounded(value: &str) -> String {
    let mut end = value.len().min(DIAGNOSTIC_LIMIT);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| component == Component::ParentDir)
}

fn display(path: &Path) -> String {
    path.display().to_string()
}
