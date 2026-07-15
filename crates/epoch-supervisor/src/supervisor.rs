use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, sync_channel},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use epoch_blob::BlobHash;
use epoch_core::{BranchId, EventActor, EventKind, EventStatus, SessionId};
use epoch_events::{EventJournal, JournalError, NewEvent};
use epoch_protocol::{
    CompletionOutcome, IngestError, Message, ProtocolError, StreamValidator, SupervisorBinding,
    ToolOutcome,
};
use epoch_storage::Store;
use nix::{
    errno::Errno,
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use rusqlite::{TransactionBehavior, params};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    ManifestError, WorkloadManifest,
    stream::{ReaderMessage, StreamError as PipeError, read_stderr, read_stdout},
};

pub const MAX_STDOUT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_STDERR_BYTES: usize = 1024 * 1024;

const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const READER_CHANNEL_CAPACITY: usize = 32;
const READER_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentTermination {
    Succeeded {
        code: i32,
    },
    NonZero {
        code: Option<i32>,
        signal: Option<i32>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOutcome {
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub termination: AgentTermination,
    pub protocol_records: usize,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
struct ExecutionIds {
    session_id: SessionId,
    branch_id: BranchId,
}

#[derive(Debug)]
pub struct DirectSupervisor {
    pub(crate) database_path: PathBuf,
    pub(crate) blob_root: PathBuf,
    pub(crate) journal: EventJournal,
}

impl DirectSupervisor {
    /// Opens or creates the trusted state directory for direct execution.
    ///
    /// # Errors
    ///
    /// Returns an error when the durable state or blob store cannot be initialized.
    pub fn open(state_root: impl AsRef<Path>) -> Result<Self, SupervisorError> {
        let requested = state_root.as_ref();
        fs::create_dir_all(requested).map_err(|error| initialization(&error))?;
        protect_state_directory(requested).map_err(|error| initialization(&error))?;
        let state_root = fs::canonicalize(requested).map_err(|error| initialization(&error))?;
        let database_path = state_root.join("state.db");
        let blob_root = state_root.join("blobs");
        Store::open(&database_path).map_err(|error| initialization(&error))?;
        let journal = EventJournal::open(&database_path, &blob_root)
            .map_err(|error| initialization(&error))?;
        Ok(Self {
            database_path,
            blob_root,
            journal,
        })
    }

    /// Executes a declared workload without shell interpolation.
    ///
    /// # Errors
    ///
    /// Returns a supervisor error for manifest, launch, protocol, persistence, or cleanup failure.
    /// A normal nonzero agent exit is returned as [`AgentTermination::NonZero`].
    pub fn run_manifest(
        &self,
        manifest_path: impl AsRef<Path>,
    ) -> Result<RunOutcome, SupervisorError> {
        let manifest = WorkloadManifest::load(manifest_path)?;
        let ids = self
            .create_execution()
            .map_err(|error| initialization(&error))?;
        let started = Instant::now();
        let result = self.run_created_execution(&manifest, ids, started);
        match result {
            Ok(outcome) => Ok(outcome),
            Err(source) => {
                self.record_supervisor_failure(ids, &source, started);
                Err(SupervisorError::Execution {
                    session_id: ids.session_id,
                    branch_id: ids.branch_id,
                    source,
                })
            }
        }
    }

    fn run_created_execution(
        &self,
        manifest: &WorkloadManifest,
        ids: ExecutionIds,
        started: Instant,
    ) -> Result<RunOutcome, ExecutionError> {
        self.append_event(
            ids,
            started,
            "supervisor.run_started",
            EventActor::Supervisor,
            EventStatus::Started,
            json!({
                "backend": "direct",
                "workload": manifest.name,
                "argument_count": manifest.arguments.len(),
            }),
        )?;

        let (mut child, stdout, stderr) = spawn_agent(manifest, ids)?;
        self.mark_session_running(ids)?;
        self.append_event(
            ids,
            started,
            "process.started",
            EventActor::Supervisor,
            EventStatus::Succeeded,
            json!({"pid": u64::from(child.id()), "backend": "direct"}),
        )?;

        let monitored = self.monitor_process(&mut child, stdout, stderr, ids, started)?;
        child.disarm();
        let termination = classify_termination(monitored.status);
        if matches!(termination, AgentTermination::Succeeded { .. }) && !monitored.complete {
            return Err(ExecutionError::MissingCompletion);
        }

        if !monitored.stderr.is_empty() {
            self.append_event(
                ids,
                started,
                "process.stderr",
                EventActor::Supervisor,
                EventStatus::Succeeded,
                json!({
                    "byte_length": monitored.stderr.len(),
                    "sha256": BlobHash::digest(&monitored.stderr),
                    "bytes": &monitored.stderr,
                }),
            )?;
        }
        let exit_status = if matches!(termination, AgentTermination::Succeeded { .. }) {
            EventStatus::Succeeded
        } else {
            EventStatus::Failed
        };
        self.append_event(
            ids,
            started,
            "process.exited",
            EventActor::Supervisor,
            exit_status,
            termination_payload(termination),
        )?;
        self.mark_terminal(ids, exit_status == EventStatus::Succeeded)?;

        Ok(RunOutcome {
            session_id: ids.session_id,
            branch_id: ids.branch_id,
            termination,
            protocol_records: monitored.protocol_records,
            stderr: monitored.stderr,
        })
    }

    fn monitor_process(
        &self,
        child: &mut ChildGuard,
        stdout: ChildStdout,
        stderr: ChildStderr,
        ids: ExecutionIds,
        started: Instant,
    ) -> Result<MonitoredProcess, ExecutionError> {
        let (sender, receiver) = sync_channel(READER_CHANNEL_CAPACITY);
        let stdout_sender = sender.clone();
        let stdout_thread = thread::spawn(move || read_stdout(stdout, &stdout_sender));
        let stderr_thread = thread::spawn(move || read_stderr(stderr, &sender));

        let monitored = self.monitor_messages(child, &receiver, ids, started);
        if monitored.is_err() {
            child.terminate_group()?;
        }
        drop(receiver);
        join_reader(stdout_thread)?;
        join_reader(stderr_thread)?;
        monitored
    }

    fn monitor_messages(
        &self,
        child: &mut ChildGuard,
        receiver: &Receiver<ReaderMessage>,
        ids: ExecutionIds,
        started: Instant,
    ) -> Result<MonitoredProcess, ExecutionError> {
        let binding = SupervisorBinding::new(ids.session_id.to_string(), ids.branch_id.to_string())
            .map_err(|source| ExecutionError::Protocol { line: 0, source })?;
        let mut validator = StreamValidator::new(binding);
        let mut stdout_finished = false;
        let mut captured_stderr = None;
        let mut status = None;
        let mut exit_observed = None;
        let mut protocol_records = 0_usize;

        loop {
            match receiver.recv_timeout(READER_POLL_INTERVAL) {
                Ok(ReaderMessage::StdoutRecord(record)) => {
                    validator
                        .accept_claims(&record.envelope)
                        .map_err(|source| ExecutionError::Ingest {
                            line: record.line,
                            source,
                        })?;
                    let payload: Value = serde_json::from_slice(&record.raw).map_err(|error| {
                        ExecutionError::Internal {
                            message: format!("validated record could not be decoded: {error}"),
                        }
                    })?;
                    self.append_event(
                        ids,
                        started,
                        record.envelope.message.kind(),
                        EventActor::Agent,
                        boundary_status(&record.envelope.message),
                        payload,
                    )?;
                    protocol_records =
                        protocol_records
                            .checked_add(1)
                            .ok_or(ExecutionError::StdoutTooLarge {
                                maximum: MAX_STDOUT_BYTES,
                            })?;
                }
                Ok(ReaderMessage::StdoutFinished) => stdout_finished = true,
                Ok(ReaderMessage::StdoutFailed(error) | ReaderMessage::StderrFailed(error)) => {
                    return Err(map_pipe_error(error));
                }
                Ok(ReaderMessage::StderrFinished(stderr)) => captured_stderr = Some(stderr),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected)
                    if stdout_finished && captured_stderr.is_some() =>
                {
                    // EOF on both pipes may become visible just before the child is reapable.
                    // Avoid a hot loop while preserving the normal exit/pipeline checks below.
                    thread::sleep(READER_POLL_INTERVAL);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ExecutionError::ReaderDisconnected);
                }
            }

            if status.is_none()
                && let Some(observed) = child.try_wait()?
            {
                status = Some(observed);
                exit_observed = Some(Instant::now());
            }
            if let Some(status) = status
                && stdout_finished
                && let Some(stderr) = captured_stderr
            {
                return Ok(MonitoredProcess {
                    status,
                    stderr,
                    protocol_records,
                    complete: validator.is_complete(),
                });
            }
            if exit_observed.is_some_and(|observed| observed.elapsed() > PIPE_DRAIN_TIMEOUT) {
                return Err(ExecutionError::PipeDrainTimeout);
            }
        }
    }

    fn append_event(
        &self,
        ids: ExecutionIds,
        started: Instant,
        kind: &str,
        actor: EventActor,
        status: EventStatus,
        payload: Value,
    ) -> Result<(), ExecutionError> {
        let kind = EventKind::new(kind).map_err(|error| ExecutionError::Persistence {
            message: error.to_string(),
        })?;
        self.journal
            .append(NewEvent {
                session_id: ids.session_id,
                branch_id: ids.branch_id,
                epoch_id: None,
                causal_parent: None,
                monotonic_ns: elapsed_ns(started),
                occurred_at_unix_ms: unix_ms().map_err(|error| persistence(&error))?,
                actor,
                kind,
                // Protocol hashes are untrusted claims embedded in `payload`. They are never
                // copied into trusted blob foreign keys without independently captured bytes.
                input_hash: None,
                output_hash: None,
                status,
                payload,
            })
            .map(|_| ())
            .map_err(|error| persistence(&error))
    }

    fn create_execution(&self) -> Result<ExecutionIds, String> {
        let ids = ExecutionIds {
            session_id: SessionId::new(),
            branch_id: BranchId::new(),
        };
        let timestamp = unix_ms()?;
        let mut store = Store::open(&self.database_path).map_err(|error| error.to_string())?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO sessions (id, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, 'created', ?2, ?2)",
                params![ids.session_id.to_string(), timestamp],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO branches \
                 (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, ?2, 'created', ?3, ?3)",
                params![
                    ids.branch_id.to_string(),
                    ids.session_id.to_string(),
                    timestamp
                ],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "UPDATE sessions SET state = 'starting', updated_at_unix_ms = ?2 WHERE id = ?1",
                params![ids.session_id.to_string(), timestamp],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "UPDATE branches SET state = 'running', updated_at_unix_ms = ?2 WHERE id = ?1",
                params![ids.branch_id.to_string(), timestamp],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(ids)
    }

    fn mark_session_running(&self, ids: ExecutionIds) -> Result<(), ExecutionError> {
        let timestamp = unix_ms().map_err(|error| persistence(&error))?;
        let store = Store::open(&self.database_path).map_err(|error| persistence(&error))?;
        let changed = store
            .connection()
            .execute(
                "UPDATE sessions \
                 SET state = 'running', updated_at_unix_ms = ?2 \
                 WHERE id = ?1 AND state = 'starting'",
                params![ids.session_id.to_string(), timestamp],
            )
            .map_err(|error| persistence(&error))?;
        if changed == 1 {
            Ok(())
        } else {
            Err(ExecutionError::Persistence {
                message: "session was not in starting state".to_owned(),
            })
        }
    }

    fn mark_terminal(&self, ids: ExecutionIds, succeeded: bool) -> Result<(), ExecutionError> {
        let timestamp = unix_ms().map_err(|error| persistence(&error))?;
        let state = if succeeded { "completed" } else { "failed" };
        let mut store = Store::open(&self.database_path).map_err(|error| persistence(&error))?;
        let transaction = store
            .connection_mut()
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| persistence(&error))?;
        transaction
            .execute(
                "UPDATE sessions SET state = ?2, updated_at_unix_ms = ?3 WHERE id = ?1",
                params![ids.session_id.to_string(), state, timestamp],
            )
            .map_err(|error| persistence(&error))?;
        transaction
            .execute(
                "UPDATE branches SET state = ?2, updated_at_unix_ms = ?3 WHERE id = ?1",
                params![ids.branch_id.to_string(), state, timestamp],
            )
            .map_err(|error| persistence(&error))?;
        transaction.commit().map_err(|error| persistence(&error))
    }

    fn record_supervisor_failure(
        &self,
        ids: ExecutionIds,
        error: &ExecutionError,
        started: Instant,
    ) {
        let _ = self.append_event(
            ids,
            started,
            "supervisor.failure",
            EventActor::Supervisor,
            EventStatus::Failed,
            json!({"category": error.category()}),
        );
        let _ = self.mark_terminal(ids, false);
    }
}

#[derive(Debug)]
struct MonitoredProcess {
    status: ExitStatus,
    stderr: Vec<u8>,
    protocol_records: usize,
    complete: bool,
}

#[derive(Debug)]
struct ChildGuard {
    child: Child,
    parent_reaped: bool,
    disarmed: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            parent_reaped: false,
            disarmed: false,
        }
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>, ExecutionError> {
        let status = self.child.try_wait().map_err(|error| io_error(&error))?;
        if status.is_some() {
            self.parent_reaped = true;
        }
        Ok(status)
    }

    fn terminate_group(&mut self) -> Result<(), ExecutionError> {
        if !self.parent_reaped
            && self
                .child
                .try_wait()
                .map_err(|error| io_error(&error))?
                .is_some()
        {
            self.parent_reaped = true;
        }
        let pid = i32::try_from(self.child.id()).map_err(|_| ExecutionError::Cleanup {
            message: "child PID does not fit process-group API".to_owned(),
        })?;
        let process_group = Pid::from_raw(pid);
        let mut signalled = killpg(process_group, Signal::SIGKILL);
        if matches!(signalled, Err(Errno::EPERM)) && !self.parent_reaped {
            // Darwin can briefly reject a group signal while its leader is exiting. Reap the
            // direct child and retry so any descendants are still addressed by the group ID.
            let _ = self.child.kill();
            self.child.wait().map_err(|error| io_error(&error))?;
            self.parent_reaped = true;
            signalled = killpg(process_group, Signal::SIGKILL);
        }
        match signalled {
            Ok(()) | Err(Errno::ESRCH) => {}
            Err(Errno::EPERM) if self.parent_reaped => {}
            Err(error) => {
                return Err(ExecutionError::Cleanup {
                    message: error.to_string(),
                });
            }
        }
        if !self.parent_reaped {
            let _ = self.child.kill();
            self.child.wait().map_err(|error| io_error(&error))?;
            self.parent_reaped = true;
        }
        self.disarmed = true;
        Ok(())
    }

    const fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.disarmed {
            let _ = self.terminate_group();
        }
    }
}

fn spawn_agent(
    manifest: &WorkloadManifest,
    ids: ExecutionIds,
) -> Result<(ChildGuard, ChildStdout, ChildStderr), ExecutionError> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt as _;

    let mut command = Command::new(&manifest.executable);
    command
        .args(&manifest.arguments)
        .current_dir(&manifest.working_directory)
        .env_clear()
        .env("EPOCH_SESSION_ID", ids.session_id.to_string())
        .env("EPOCH_BRANCH_ID", ids.branch_id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(|error| ExecutionError::Spawn {
        message: error.to_string(),
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ExecutionError::Internal {
            message: "spawned child has no stdout pipe".to_owned(),
        })?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ExecutionError::Internal {
            message: "spawned child has no stderr pipe".to_owned(),
        })?;
    Ok((ChildGuard::new(child), stdout, stderr))
}

fn join_reader(thread: JoinHandle<()>) -> Result<(), ExecutionError> {
    thread.join().map_err(|_| ExecutionError::ReaderPanicked)
}

fn map_pipe_error(error: PipeError) -> ExecutionError {
    match error {
        PipeError::Protocol { line, source } => ExecutionError::Protocol { line, source },
        PipeError::PartialFinalRecord { line, bytes } => {
            ExecutionError::PartialFinalRecord { line, bytes }
        }
        PipeError::StdoutTooLarge => ExecutionError::StdoutTooLarge {
            maximum: MAX_STDOUT_BYTES,
        },
        PipeError::StderrTooLarge => ExecutionError::StderrTooLarge {
            maximum: MAX_STDERR_BYTES,
        },
        PipeError::Io(error) => io_error(&error),
    }
}

const fn boundary_status(message: &Message) -> EventStatus {
    match message {
        Message::ModelRequest(_) | Message::ToolCall(_) => EventStatus::Started,
        Message::ToolResult(result) => match result.outcome {
            ToolOutcome::Succeeded => EventStatus::Succeeded,
            ToolOutcome::Failed => EventStatus::Failed,
            ToolOutcome::Denied => EventStatus::Denied,
        },
        Message::Completion(completion) => match completion.outcome {
            CompletionOutcome::Succeeded => EventStatus::Succeeded,
            CompletionOutcome::Failed | CompletionOutcome::Cancelled => EventStatus::Failed,
        },
        Message::AgentStart(_)
        | Message::ContextUpdate(_)
        | Message::ModelResponse(_)
        | Message::SafePoint(_) => EventStatus::Succeeded,
    }
}

fn classify_termination(status: ExitStatus) -> AgentTermination {
    if status.success() {
        AgentTermination::Succeeded {
            code: status.code().unwrap_or(0),
        }
    } else {
        AgentTermination::NonZero {
            code: status.code(),
            signal: exit_signal(status),
        }
    }
}

#[cfg(unix)]
fn exit_signal(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
const fn exit_signal(_status: &ExitStatus) -> Option<i32> {
    None
}

fn termination_payload(termination: AgentTermination) -> Value {
    match termination {
        AgentTermination::Succeeded { code } => json!({"outcome": "succeeded", "code": code}),
        AgentTermination::NonZero { code, signal } => {
            json!({"outcome": "nonzero", "code": code, "signal": signal})
        }
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(9_223_372_036_854_775_807)
}

fn unix_ms() -> Result<i64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?;
    i64::try_from(duration.as_millis()).map_err(|_| "wall clock does not fit i64".to_owned())
}

fn protect_state_directory(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

fn initialization(error: &impl ToString) -> SupervisorError {
    SupervisorError::Initialization {
        message: error.to_string(),
    }
}

fn persistence(error: &impl ToString) -> ExecutionError {
    ExecutionError::Persistence {
        message: error.to_string(),
    }
}

fn io_error(error: &std::io::Error) -> ExecutionError {
    ExecutionError::Io {
        message: error.to_string(),
    }
}

#[derive(Debug, Error)]
pub enum SupervisorError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("session {session_id} branch {branch_id} failed in the supervisor: {source}")]
    Execution {
        session_id: SessionId,
        branch_id: BranchId,
        #[source]
        source: ExecutionError,
    },
    #[error("supervisor initialization failed: {message}")]
    Initialization { message: String },
}

impl SupervisorError {
    #[must_use]
    pub const fn session_id(&self) -> Option<SessionId> {
        match self {
            Self::Execution { session_id, .. } => Some(*session_id),
            Self::Manifest(_) | Self::Initialization { .. } => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("agent protocol record {line} is invalid: {source}")]
    Protocol {
        line: usize,
        #[source]
        source: ProtocolError,
    },
    #[error("agent protocol stream failed at record {line}: {source}")]
    Ingest {
        line: usize,
        #[source]
        source: IngestError,
    },
    #[error("stdout ended with a partial record at line {line} ({bytes} bytes)")]
    PartialFinalRecord { line: usize, bytes: usize },
    #[error("stdout exceeded its {maximum}-byte limit")]
    StdoutTooLarge { maximum: usize },
    #[error("stderr exceeded its {maximum}-byte limit")]
    StderrTooLarge { maximum: usize },
    #[error("successful agent exit omitted a completion record")]
    MissingCompletion,
    #[error("agent process could not be launched: {message}")]
    Spawn { message: String },
    #[error("supervisor I/O failed: {message}")]
    Io { message: String },
    #[error("event or lifecycle persistence failed: {message}")]
    Persistence { message: String },
    #[error("stdout or stderr reader disconnected without a terminal message")]
    ReaderDisconnected,
    #[error("stdout or stderr reader panicked")]
    ReaderPanicked,
    #[error("child exited but its output pipes did not close within the drain timeout")]
    PipeDrainTimeout,
    #[error("child process-group cleanup failed: {message}")]
    Cleanup { message: String },
    #[error("internal supervisor invariant failed: {message}")]
    Internal { message: String },
}

impl ExecutionError {
    const fn category(&self) -> &'static str {
        match self {
            Self::Protocol { .. } | Self::Ingest { .. } => "protocol",
            Self::PartialFinalRecord { .. } => "partial_stdout_record",
            Self::StdoutTooLarge { .. } => "stdout_limit",
            Self::StderrTooLarge { .. } => "stderr_limit",
            Self::MissingCompletion => "missing_completion",
            Self::Spawn { .. } => "spawn",
            Self::Io { .. } => "io",
            Self::Persistence { .. } => "persistence",
            Self::ReaderDisconnected | Self::ReaderPanicked => "reader",
            Self::PipeDrainTimeout => "pipe_drain_timeout",
            Self::Cleanup { .. } => "cleanup",
            Self::Internal { .. } => "internal",
        }
    }
}

impl From<JournalError> for ExecutionError {
    fn from(error: JournalError) -> Self {
        persistence(&error)
    }
}
