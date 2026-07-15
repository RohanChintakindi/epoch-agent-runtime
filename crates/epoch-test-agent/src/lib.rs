//! Deterministic workload used to exercise Epoch's execution and checkpoint boundaries.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::Command,
};

use clap::ValueEnum;
use epoch_blob::BlobHash;
use epoch_checkpoint::{APPLICATION_CONTEXT_SCHEMA_VERSION, ApplicationContext, ResumeCursors};
use epoch_protocol::{
    AgentStart, Completion, CompletionOutcome, ContextUpdate, Envelope, Extensions, Message,
    ModelRequest, ModelResponse, ProtocolError, SafePoint, ToolCall, ToolOutcome, ToolResult,
    encode_line,
};
use serde::Serialize;
use thiserror::Error;

pub const DEFAULT_MEMORY_BYTES: usize = 64 * 1024;
pub const MAX_MEMORY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    Full,
    Files,
    Memory,
    Child,
    Network,
}

impl Scenario {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Files => "files",
            Self::Memory => "memory",
            Self::Child => "child",
            Self::Network => "network",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkloadConfig {
    pub seed: u64,
    pub scenario: Scenario,
    pub workspace: PathBuf,
    pub memory_bytes: usize,
    pub crash_at: Option<CrashPoint>,
    pub execution_binding: Option<ExecutionBinding>,
}

impl WorkloadConfig {
    #[must_use]
    pub fn new(seed: u64, scenario: Scenario, workspace: PathBuf) -> Self {
        Self {
            seed,
            scenario,
            workspace,
            memory_bytes: DEFAULT_MEMORY_BYTES,
            crash_at: None,
            execution_binding: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionBinding {
    pub session_id: String,
    pub branch_id: String,
}

impl ExecutionBinding {
    #[must_use]
    pub fn new(session_id: impl Into<String>, branch_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            branch_id: branch_id.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum CrashPoint {
    AfterModel,
    AfterFirstTool,
    AfterSafePoint,
}

impl CrashPoint {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AfterModel => "after_model",
            Self::AfterFirstTool => "after_first_tool",
            Self::AfterSafePoint => "after_safe_point",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MemoryState {
    pub bytes: usize,
    pub content_hash: BlobHash,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ChildState {
    pub exit_code: i32,
    pub stdout_hash: BlobHash,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NetworkState {
    pub request_hash: BlobHash,
    pub response_hash: BlobHash,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NormalizedState {
    pub seed: u64,
    pub scenario: Scenario,
    pub model_response_hash: BlobHash,
    pub files: BTreeMap<String, BlobHash>,
    pub memory: Option<MemoryState>,
    pub child: Option<ChildState>,
    pub network: Option<NetworkState>,
    pub completed_tools: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RunSummary {
    pub state: NormalizedState,
    pub state_hash: BlobHash,
    pub normalized_trace_hash: BlobHash,
    pub event_count: u64,
}

impl RunSummary {
    /// Convert a completed deterministic run into observable resumable application state.
    ///
    /// # Errors
    ///
    /// Returns an error when the run summary does not contain a completed safe-point cursor.
    pub fn to_application_context(&self) -> Result<ApplicationContext, WorkloadError> {
        let boundary_sequence = self.event_count.checked_sub(2).ok_or_else(|| {
            WorkloadError::CheckpointAdapter(
                "completed trace does not contain safe-point and completion records".to_owned(),
            )
        })?;
        let completed_count = u64::try_from(self.state.completed_tools.len()).map_err(|error| {
            WorkloadError::CheckpointAdapter(format!("completed tool count is invalid: {error}"))
        })?;
        let tool_registry = self
            .state
            .completed_tools
            .iter()
            .map(|tool| (tool.clone(), "fixture-v1".to_owned()))
            .collect();
        Ok(ApplicationContext {
            schema_version: APPLICATION_CONTEXT_SCHEMA_VERSION,
            safe_point_id: deterministic_id("safe-point", self.state.seed, self.state.scenario),
            deterministic_seed: self.state.seed,
            context_revision: 1,
            cursors: ResumeCursors {
                boundary_sequence,
                message_cursor: 2,
                tool_cursor: completed_count,
                task_cursor: completed_count,
            },
            model_identifier: "recorded-model-v1".to_owned(),
            tool_registry,
            messages: Vec::new(),
            pending_tasks: Vec::new(),
            pending_model_request_ids: Vec::new(),
            pending_tool_call_ids: Vec::new(),
            user_visible_summary_hash: None,
        })
    }
}

#[derive(Debug, Error)]
pub enum WorkloadError {
    #[error("invalid workload configuration: {0}")]
    InvalidConfig(String),
    #[error("agent boundary protocol failure: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("workload I/O failure: {0}")]
    Io(#[from] std::io::Error),
    #[error("recorded model fixture is invalid: {0}")]
    ModelFixture(String),
    #[error("child process exited unsuccessfully with code {0}")]
    ChildFailed(i32),
    #[error("normalized state cannot be encoded: {0}")]
    StateEncoding(String),
    #[error("injected crash at {point:?}")]
    InjectedCrash { point: CrashPoint },
    #[error("cannot build application checkpoint context: {0}")]
    CheckpointAdapter(String),
}

/// Execute a deterministic workload and write its agent boundary history to `output`.
///
/// # Errors
///
/// Returns an error when configuration is unsafe, an exercised mechanism fails, or a boundary
/// record cannot be encoded and flushed.
pub fn run_workload(
    config: &WorkloadConfig,
    output: &mut impl Write,
) -> Result<RunSummary, WorkloadError> {
    validate_config(config)?;
    fs::create_dir_all(&config.workspace)?;

    let response = recorded_response(config.scenario)?;
    let mut state = NormalizedState {
        seed: config.seed,
        scenario: config.scenario,
        model_response_hash: sha256(response.as_bytes()),
        files: BTreeMap::new(),
        memory: None,
        child: None,
        network: None,
        completed_tools: Vec::new(),
    };
    let mut emitter = Emitter::new(output);
    let mut random = SplitMix64::new(config.seed);

    let (session_id, branch_id) = config.execution_binding.as_ref().map_or_else(
        || {
            (
                deterministic_id("session", config.seed, config.scenario),
                deterministic_id("branch", config.seed, config.scenario),
            )
        },
        |binding| (binding.session_id.clone(), binding.branch_id.clone()),
    );
    emitter.emit(Message::AgentStart(AgentStart {
        agent_id: deterministic_id("agent", config.seed, config.scenario),
        session_id,
        branch_id,
        extensions: Extensions::new(),
    }))?;

    let prompt = format!("scenario={} seed={}", config.scenario.as_str(), config.seed);
    emitter.emit(Message::ContextUpdate(ContextUpdate {
        revision: 0,
        context_hash: sha256(prompt.as_bytes()),
        extensions: Extensions::new(),
    }))?;
    let request_id = deterministic_id("model-request", config.seed, config.scenario);
    emitter.emit(Message::ModelRequest(ModelRequest {
        request_id: request_id.clone(),
        model: "recorded-model-v1".to_owned(),
        input_hash: sha256(prompt.as_bytes()),
        extensions: Extensions::new(),
    }))?;
    emitter.emit(Message::ModelResponse(ModelResponse {
        request_id,
        output_hash: state.model_response_hash.clone(),
        extensions: Extensions::new(),
    }))?;
    maybe_crash(config, CrashPoint::AfterModel)?;

    if runs_files(config.scenario) {
        run_file_tools(config, &mut random, &mut state, &mut emitter)?;
    }

    let mut memory_buffer = None;
    if runs_memory(config.scenario) {
        memory_buffer = Some(run_memory_tool(
            config,
            &mut random,
            &mut state,
            &mut emitter,
        )?);
    }
    if runs_child(config.scenario) {
        run_child_tool(config, &mut state, &mut emitter)?;
    }
    if runs_network(config.scenario) {
        run_network_tool(config, &mut state, &mut emitter)?;
    }

    if let (Some(buffer), Some(memory_state)) = (&memory_buffer, &state.memory) {
        debug_assert_eq!(buffer.len(), memory_state.bytes);
        debug_assert_eq!(sha256(buffer), memory_state.content_hash);
    }

    let state_hash = normalized_state_hash(&state)?;
    emitter.emit(Message::ContextUpdate(ContextUpdate {
        revision: 1,
        context_hash: state_hash.clone(),
        extensions: Extensions::new(),
    }))?;
    emitter.emit(Message::SafePoint(SafePoint {
        safe_point_id: deterministic_id("safe-point", config.seed, config.scenario),
        context_hash: state_hash.clone(),
        extensions: Extensions::new(),
    }))?;
    maybe_crash(config, CrashPoint::AfterSafePoint)?;
    emitter.emit(Message::Completion(Completion {
        outcome: CompletionOutcome::Succeeded,
        output_hash: Some(state_hash.clone()),
        extensions: Extensions::new(),
    }))?;

    let (normalized_trace_hash, event_count) = emitter.finish();
    Ok(RunSummary {
        state,
        state_hash,
        normalized_trace_hash,
        event_count,
    })
}

fn validate_config(config: &WorkloadConfig) -> Result<(), WorkloadError> {
    if config.memory_bytes == 0 || config.memory_bytes > MAX_MEMORY_BYTES {
        return Err(WorkloadError::InvalidConfig(format!(
            "memory_bytes must be between 1 and {MAX_MEMORY_BYTES}"
        )));
    }
    Ok(())
}

fn recorded_response(scenario: Scenario) -> Result<String, WorkloadError> {
    let responses: BTreeMap<String, String> =
        serde_json::from_str(include_str!("../fixtures/recorded-model-responses.json"))
            .map_err(|error| WorkloadError::ModelFixture(error.to_string()))?;
    responses
        .get(scenario.as_str())
        .cloned()
        .ok_or_else(|| WorkloadError::ModelFixture(format!("missing `{}`", scenario.as_str())))
}

fn run_file_tools<W: Write>(
    config: &WorkloadConfig,
    random: &mut SplitMix64,
    state: &mut NormalizedState,
    emitter: &mut Emitter<'_, W>,
) -> Result<(), WorkloadError> {
    let relative_path = "artifact.txt";
    let artifact_path = config.workspace.join(relative_path);
    let initial = format!(
        "epoch deterministic artifact\nseed={}\nnonce={:016x}\n",
        config.seed,
        random.next_u64()
    );
    emitter.emit(Message::ToolCall(ToolCall {
        call_id: deterministic_id("file-create", config.seed, config.scenario),
        tool: "file.create".to_owned(),
        input_hash: sha256(format!("{relative_path}\0{initial}").as_bytes()),
        extensions: Extensions::new(),
    }))?;
    fs::write(&artifact_path, initial.as_bytes())?;
    let initial_hash = sha256(initial.as_bytes());
    state
        .files
        .insert(relative_path.to_owned(), initial_hash.clone());
    state.completed_tools.push("file.create".to_owned());
    emitter.emit(Message::ToolResult(ToolResult {
        call_id: deterministic_id("file-create", config.seed, config.scenario),
        outcome: ToolOutcome::Succeeded,
        output_hash: Some(initial_hash),
        extensions: Extensions::new(),
    }))?;
    maybe_crash_after_tool(config, state)?;

    let mutation = format!("mutation={:016x}\n", random.next_u64());
    emitter.emit(Message::ToolCall(ToolCall {
        call_id: deterministic_id("file-append", config.seed, config.scenario),
        tool: "file.append".to_owned(),
        input_hash: sha256(format!("{relative_path}\0{mutation}").as_bytes()),
        extensions: Extensions::new(),
    }))?;
    OpenOptions::new()
        .append(true)
        .open(&artifact_path)?
        .write_all(mutation.as_bytes())?;
    let final_contents = fs::read(&artifact_path)?;
    let final_hash = sha256(&final_contents);
    state
        .files
        .insert(relative_path.to_owned(), final_hash.clone());
    state.completed_tools.push("file.append".to_owned());
    emitter.emit(Message::ToolResult(ToolResult {
        call_id: deterministic_id("file-append", config.seed, config.scenario),
        outcome: ToolOutcome::Succeeded,
        output_hash: Some(final_hash),
        extensions: Extensions::new(),
    }))?;
    maybe_crash_after_tool(config, state)?;
    Ok(())
}

fn run_memory_tool<W: Write>(
    config: &WorkloadConfig,
    random: &mut SplitMix64,
    state: &mut NormalizedState,
    emitter: &mut Emitter<'_, W>,
) -> Result<Vec<u8>, WorkloadError> {
    let call_id = deterministic_id("memory", config.seed, config.scenario);
    emitter.emit(Message::ToolCall(ToolCall {
        call_id: call_id.clone(),
        tool: "memory.allocate".to_owned(),
        input_hash: sha256(config.memory_bytes.to_string().as_bytes()),
        extensions: Extensions::new(),
    }))?;
    let mut buffer = vec![0_u8; config.memory_bytes];
    random.fill(&mut buffer);
    let content_hash = sha256(&buffer);
    state.memory = Some(MemoryState {
        bytes: buffer.len(),
        content_hash: content_hash.clone(),
    });
    state.completed_tools.push("memory.allocate".to_owned());
    emitter.emit(Message::ToolResult(ToolResult {
        call_id,
        outcome: ToolOutcome::Succeeded,
        output_hash: Some(content_hash),
        extensions: Extensions::new(),
    }))?;
    maybe_crash_after_tool(config, state)?;
    Ok(buffer)
}

fn run_child_tool<W: Write>(
    config: &WorkloadConfig,
    state: &mut NormalizedState,
    emitter: &mut Emitter<'_, W>,
) -> Result<(), WorkloadError> {
    let call_id = deterministic_id("child", config.seed, config.scenario);
    let child_script = "printf epoch-child-v1";
    emitter.emit(Message::ToolCall(ToolCall {
        call_id: call_id.clone(),
        tool: "process.spawn".to_owned(),
        input_hash: sha256(child_script.as_bytes()),
        extensions: Extensions::new(),
    }))?;
    let result = Command::new("/bin/sh")
        .args(["-c", child_script])
        .env_clear()
        .current_dir(&config.workspace)
        .output()?;
    let exit_code = result.status.code().unwrap_or(-1);
    if !result.status.success() {
        return Err(WorkloadError::ChildFailed(exit_code));
    }
    let output_hash = sha256(&result.stdout);
    state.child = Some(ChildState {
        exit_code,
        stdout_hash: output_hash.clone(),
    });
    state.completed_tools.push("process.spawn".to_owned());
    emitter.emit(Message::ToolResult(ToolResult {
        call_id,
        outcome: ToolOutcome::Succeeded,
        output_hash: Some(output_hash),
        extensions: Extensions::new(),
    }))?;
    maybe_crash_after_tool(config, state)?;
    Ok(())
}

fn run_network_tool<W: Write>(
    config: &WorkloadConfig,
    state: &mut NormalizedState,
    emitter: &mut Emitter<'_, W>,
) -> Result<(), WorkloadError> {
    let request = format!("ping:{:016x}", config.seed).into_bytes();
    let response = format!("pong:{:016x}", config.seed).into_bytes();
    let call_id = deterministic_id("network", config.seed, config.scenario);
    emitter.emit(Message::ToolCall(ToolCall {
        call_id: call_id.clone(),
        tool: "network.loopback".to_owned(),
        input_hash: sha256(&request),
        extensions: Extensions::new(),
    }))?;

    let observed = loopback_exchange(&request, &response)?;
    let response_hash = sha256(&observed);
    state.network = Some(NetworkState {
        request_hash: sha256(&request),
        response_hash: response_hash.clone(),
    });
    state.completed_tools.push("network.loopback".to_owned());
    emitter.emit(Message::ToolResult(ToolResult {
        call_id,
        outcome: ToolOutcome::Succeeded,
        output_hash: Some(response_hash),
        extensions: Extensions::new(),
    }))?;
    maybe_crash_after_tool(config, state)?;
    Ok(())
}

fn maybe_crash_after_tool(
    config: &WorkloadConfig,
    state: &NormalizedState,
) -> Result<(), WorkloadError> {
    if state.completed_tools.len() == 1 {
        maybe_crash(config, CrashPoint::AfterFirstTool)?;
    }
    Ok(())
}

fn maybe_crash(config: &WorkloadConfig, point: CrashPoint) -> Result<(), WorkloadError> {
    if config.crash_at == Some(point) {
        return Err(WorkloadError::InjectedCrash { point });
    }
    Ok(())
}

fn loopback_exchange(request: &[u8], response: &[u8]) -> Result<Vec<u8>, WorkloadError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let address = listener.local_addr()?;
    let mut client = TcpStream::connect(address)?;
    let (mut server, _) = listener.accept()?;
    client.write_all(request)?;
    let mut received = vec![0_u8; request.len()];
    server.read_exact(&mut received)?;
    if received != request {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "loopback request differed from fixture",
        )
        .into());
    }
    server.write_all(response)?;
    let mut observed = vec![0_u8; response.len()];
    client.read_exact(&mut observed)?;
    Ok(observed)
}

fn normalized_state_hash(state: &NormalizedState) -> Result<BlobHash, WorkloadError> {
    let encoded = serde_json::to_vec(state)
        .map_err(|error| WorkloadError::StateEncoding(error.to_string()))?;
    Ok(sha256(&encoded))
}

fn deterministic_id(prefix: &str, seed: u64, scenario: Scenario) -> String {
    format!("{prefix}-{}-{seed:016x}", scenario.as_str())
}

fn sha256(bytes: &[u8]) -> BlobHash {
    BlobHash::digest(bytes)
}

const fn runs_files(scenario: Scenario) -> bool {
    matches!(scenario, Scenario::Full | Scenario::Files)
}

const fn runs_memory(scenario: Scenario) -> bool {
    matches!(scenario, Scenario::Full | Scenario::Memory)
}

const fn runs_child(scenario: Scenario) -> bool {
    matches!(scenario, Scenario::Full | Scenario::Child)
}

const fn runs_network(scenario: Scenario) -> bool {
    matches!(scenario, Scenario::Full | Scenario::Network)
}

struct Emitter<'a, W> {
    output: &'a mut W,
    sequence: u64,
    normalized_trace: Vec<u8>,
}

impl<'a, W: Write> Emitter<'a, W> {
    fn new(output: &'a mut W) -> Self {
        Self {
            output,
            sequence: 0,
            normalized_trace: Vec::new(),
        }
    }

    fn emit(&mut self, message: Message) -> Result<(), WorkloadError> {
        let encoded = encode_line(&Envelope::new(self.sequence, message))?;
        self.output.write_all(encoded.as_bytes())?;
        self.output.flush()?;
        self.normalized_trace.extend_from_slice(encoded.as_bytes());
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| WorkloadError::InvalidConfig("event sequence overflow".to_owned()))?;
        Ok(())
    }

    fn finish(self) -> (BlobHash, u64) {
        (BlobHash::digest(&self.normalized_trace), self.sequence)
    }
}

struct SplitMix64(u64);

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn fill(&mut self, destination: &mut [u8]) {
        for chunk in destination.chunks_mut(8) {
            let bytes = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
    }
}
