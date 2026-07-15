use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, sync_channel},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use epoch_blob::BlobHash;
use epoch_core::{BranchId, EventActor, EventId, EventKind, EventStatus, SessionId};
use epoch_events::{EventJournal, EventQuery, JournalError, NewEvent};
use epoch_proc::{CollectorLimits, ProcCollection, collect_live};
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
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct BranchStatus {
    pub branch_id: BranchId,
    pub state: String,
    pub next_event_sequence: u64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SessionStatusReport {
    pub session_id: SessionId,
    pub state: String,
    pub policy_revision: u64,
    pub revision: u64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub branches: Vec<BranchStatus>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventPageRequest {
    pub session_id: SessionId,
    pub branch_id: Option<BranchId>,
    pub offset: u64,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ObservedEvent {
    pub event_id: EventId,
    pub sequence: u64,
    pub session_id: SessionId,
    pub branch_id: BranchId,
    pub monotonic_ns: u64,
    pub occurred_at_unix_ms: i64,
    pub actor: EventActor,
    pub kind: String,
    pub status: EventStatus,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventPageReport {
    pub session_id: SessionId,
    pub branch_id: Option<BranchId>,
    pub offset: u64,
    pub limit: usize,
    pub has_more: bool,
    pub next_offset: Option<u64>,
    pub events: Vec<ObservedEvent>,
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

    /// Opens trusted state without creating a new runtime when the requested state is absent.
    ///
    /// # Errors
    ///
    /// Returns [`InspectionError::StateNotFound`] when there is no existing database, and a
    /// state-unavailable error when the trusted database cannot be validated.
    pub fn open_existing(state_root: impl AsRef<Path>) -> Result<Self, InspectionError> {
        let requested = state_root.as_ref();
        let database = requested.join("state.db");
        if !requested.is_dir() || !database.is_file() {
            return Err(InspectionError::StateNotFound {
                path: requested.to_path_buf(),
            });
        }
        let state_root = fs::canonicalize(requested).map_err(|error| unavailable(&error))?;
        let database_path = state_root.join("state.db");
        let blob_root = state_root.join("blobs");
        Store::open(&database_path).map_err(|error| unavailable(&error))?;
        let journal =
            EventJournal::open(&database_path, &blob_root).map_err(|error| unavailable(&error))?;
        Ok(Self {
            database_path,
            blob_root,
            journal,
        })
    }

    /// Reads a durable session and its branches from trusted state.
    ///
    /// # Errors
    ///
    /// Returns a typed not-found or state-corruption error.
    pub fn session_status(
        &self,
        session_id: SessionId,
    ) -> Result<SessionStatusReport, InspectionError> {
        let store = Store::open(&self.database_path).map_err(|error| unavailable(&error))?;
        let session = store
            .connection()
            .query_row(
                "SELECT state, policy_revision, revision, created_at_unix_ms, updated_at_unix_ms \
                 FROM sessions WHERE id = ?1",
                [session_id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| unavailable(&error))?
            .ok_or(InspectionError::SessionNotFound { session_id })?;
        validate_session_state(&session.0)?;

        let mut statement = store
            .connection()
            .prepare(
                "SELECT id, state, next_event_sequence, created_at_unix_ms, updated_at_unix_ms \
                 FROM branches WHERE session_id = ?1 ORDER BY created_at_unix_ms ASC, id ASC",
            )
            .map_err(|error| unavailable(&error))?;
        let rows = statement
            .query_map([session_id.to_string()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(|error| unavailable(&error))?;
        let stored_branches = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| unavailable(&error))?;
        let mut branches = Vec::with_capacity(stored_branches.len());
        for (id, state, next_event_sequence, created_at, updated_at) in stored_branches {
            validate_branch_state(&state)?;
            branches.push(BranchStatus {
                branch_id: parse_stored("branches.id", &id)?,
                state,
                next_event_sequence: nonnegative(
                    "branches.next_event_sequence",
                    next_event_sequence,
                )?,
                created_at_unix_ms: nonnegative_i64("branches.created_at_unix_ms", created_at)?,
                updated_at_unix_ms: nonnegative_i64("branches.updated_at_unix_ms", updated_at)?,
            });
        }
        Ok(SessionStatusReport {
            session_id,
            state: session.0,
            policy_revision: nonnegative("sessions.policy_revision", session.1)?,
            revision: nonnegative("sessions.revision", session.2)?,
            created_at_unix_ms: nonnegative_i64("sessions.created_at_unix_ms", session.3)?,
            updated_at_unix_ms: nonnegative_i64("sessions.updated_at_unix_ms", session.4)?,
            branches,
        })
    }

    /// Reads a bounded, deterministic page of durable events and verifies externalized payloads.
    ///
    /// # Errors
    ///
    /// Returns typed invalid-scope/page errors or a trusted-state error when stored data cannot be
    /// decoded and verified.
    pub fn events(&self, request: EventPageRequest) -> Result<EventPageReport, InspectionError> {
        let mut query = EventQuery::for_session(request.session_id);
        query.branch_id = request.branch_id;
        let page = self
            .journal
            .query_page(&query, request.offset, request.limit)
            .map_err(InspectionError::from_journal)?;
        let mut events = Vec::with_capacity(page.events.len());
        for event in page.events {
            let payload = self
                .journal
                .read_payload(&event)
                .map_err(InspectionError::from_journal)?;
            events.push(ObservedEvent {
                event_id: event.event_id,
                sequence: event.sequence,
                session_id: event.session_id,
                branch_id: event.branch_id,
                monotonic_ns: event.monotonic_ns,
                occurred_at_unix_ms: event.occurred_at_unix_ms,
                actor: event.actor,
                kind: event.kind.to_string(),
                status: event.status,
                payload,
            });
        }
        let next_offset = if page.has_more {
            let count =
                u64::try_from(events.len()).map_err(|_| InspectionError::InvalidOffset {
                    offset: request.offset,
                })?;
            Some(
                request
                    .offset
                    .checked_add(count)
                    .ok_or(InspectionError::InvalidOffset {
                        offset: request.offset,
                    })?,
            )
        } else {
            None
        };
        Ok(EventPageReport {
            session_id: request.session_id,
            branch_id: request.branch_id,
            offset: request.offset,
            limit: request.limit,
            has_more: page.has_more,
            next_offset,
            events,
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
        let working_directory =
            manifest
                .working_directory
                .to_str()
                .ok_or_else(|| ExecutionError::Internal {
                    message: "declared workload workspace is not valid UTF-8".to_owned(),
                })?;
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
                "working_directory": working_directory,
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

        let process_collection = collect_live(child.id(), CollectorLimits::default());
        let collection_status = if matches!(&process_collection, ProcCollection::Collected(_)) {
            EventStatus::Succeeded
        } else {
            EventStatus::Unknown
        };
        self.append_event(
            ids,
            started,
            "process.manifest",
            EventActor::Supervisor,
            collection_status,
            serde_json::to_value(process_collection).map_err(|error| ExecutionError::Internal {
                message: format!("process manifest could not be encoded: {error}"),
            })?,
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

fn unavailable(error: &impl ToString) -> InspectionError {
    InspectionError::StateUnavailable {
        message: error.to_string(),
    }
}

fn parse_stored<Id>(field: &'static str, value: &str) -> Result<Id, InspectionError>
where
    Id: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| InspectionError::InvalidStoredValue {
            field,
            value: value.to_owned(),
        })
}

fn nonnegative(field: &'static str, value: i64) -> Result<u64, InspectionError> {
    u64::try_from(value).map_err(|_| InspectionError::InvalidStoredValue {
        field,
        value: value.to_string(),
    })
}

fn nonnegative_i64(field: &'static str, value: i64) -> Result<i64, InspectionError> {
    if value >= 0 {
        Ok(value)
    } else {
        Err(InspectionError::InvalidStoredValue {
            field,
            value: value.to_string(),
        })
    }
}

fn validate_session_state(value: &str) -> Result<(), InspectionError> {
    if matches!(
        value,
        "created"
            | "starting"
            | "running"
            | "suspended"
            | "checkpointing"
            | "restoring"
            | "completed"
            | "failed"
    ) {
        Ok(())
    } else {
        Err(InspectionError::InvalidStoredValue {
            field: "sessions.state",
            value: value.to_owned(),
        })
    }
}

fn validate_branch_state(value: &str) -> Result<(), InspectionError> {
    if matches!(
        value,
        "created" | "running" | "suspended" | "completed" | "promoted" | "abandoned" | "failed"
    ) {
        Ok(())
    } else {
        Err(InspectionError::InvalidStoredValue {
            field: "branches.state",
            value: value.to_owned(),
        })
    }
}

#[derive(Debug, Error)]
pub enum InspectionError {
    #[error("Epoch state does not exist at {path}")]
    StateNotFound { path: PathBuf },
    #[error("session {session_id} does not exist")]
    SessionNotFound { session_id: SessionId },
    #[error("branch {branch_id} does not exist")]
    BranchNotFound { branch_id: BranchId },
    #[error("branch {branch_id} belongs to a different session")]
    BranchSessionMismatch { branch_id: BranchId },
    #[error("event limit must be between 1 and {maximum}, got {received}")]
    InvalidLimit { received: usize, maximum: usize },
    #[error("event offset cannot be represented safely: {offset}")]
    InvalidOffset { offset: u64 },
    #[error("trusted state field {field} has invalid value {value:?}")]
    InvalidStoredValue { field: &'static str, value: String },
    #[error("trusted state is unavailable: {message}")]
    StateUnavailable { message: String },
}

impl InspectionError {
    fn from_journal(error: JournalError) -> Self {
        match error {
            JournalError::SessionNotFound { session_id } => Self::SessionNotFound { session_id },
            JournalError::BranchNotFound { branch_id } => Self::BranchNotFound { branch_id },
            JournalError::BranchSessionMismatch { branch_id, .. } => {
                Self::BranchSessionMismatch { branch_id }
            }
            JournalError::InvalidPageLimit { received, maximum } => {
                Self::InvalidLimit { received, maximum }
            }
            JournalError::NumericOutOfRange {
                field: "event_offset",
                value,
            } => Self::InvalidOffset { offset: value },
            other => unavailable(&other),
        }
    }

    #[must_use]
    pub const fn is_user_error(&self) -> bool {
        matches!(
            self,
            Self::StateNotFound { .. }
                | Self::SessionNotFound { .. }
                | Self::BranchNotFound { .. }
                | Self::BranchSessionMismatch { .. }
                | Self::InvalidLimit { .. }
                | Self::InvalidOffset { .. }
        )
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
