//! Versioned JSONL messages exchanged between an agent and the Epoch supervisor.

mod unique_json;

use epoch_blob::{BlobHash, BlobStore};
use std::collections::HashSet;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use thiserror::Error;

pub const CURRENT_PROTOCOL_VERSION: u16 = 1;
pub const MAX_JSONL_BYTES: usize = 1024 * 1024;
pub const MAX_SEQUENCE: u64 = i64::MAX as u64;
pub const MAX_CONTEXT_REVISION: u64 = i64::MAX as u64;
pub const MAX_IDENTIFIER_BYTES: usize = 255;
pub const MAX_NAME_BYTES: usize = 128;

pub type Extensions = Map<String, Value>;

#[derive(Clone, Debug, PartialEq)]
pub struct Envelope {
    pub protocol_version: u16,
    pub sequence: u64,
    pub message: Message,
    pub extensions: Extensions,
}

impl Envelope {
    #[must_use]
    pub fn new(sequence: u64, message: Message) -> Self {
        Self {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            sequence,
            message,
            extensions: Extensions::new(),
        }
    }

    /// Returns every content-addressed blob referenced by this record.
    #[must_use]
    pub fn referenced_hashes(&self) -> Vec<&BlobHash> {
        match &self.message {
            Message::AgentStart(_) => Vec::new(),
            Message::ContextUpdate(payload) => vec![&payload.context_hash],
            Message::ModelRequest(payload) => vec![&payload.input_hash],
            Message::ModelResponse(payload) => vec![&payload.output_hash],
            Message::ToolCall(payload) => vec![&payload.input_hash],
            Message::ToolResult(payload) => payload.output_hash.iter().collect(),
            Message::SafePoint(payload) => vec![&payload.context_hash],
            Message::Completion(payload) => payload.output_hash.iter().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlobReferenceStatus {
    Missing,
    Unverified,
    Verified,
}

/// Trusted lookup used immediately before a supervisor acknowledges a boundary record.
pub trait BlobReferenceResolver {
    fn status(&self, hash: &BlobHash) -> BlobReferenceStatus;
}

impl BlobReferenceResolver for BlobStore {
    fn status(&self, hash: &BlobHash) -> BlobReferenceStatus {
        match self.read(hash) {
            Ok(_) => BlobReferenceStatus::Verified,
            Err(epoch_blob::BlobError::NotFound(_)) => BlobReferenceStatus::Missing,
            Err(_) => BlobReferenceStatus::Unverified,
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ReferenceError {
    #[error("referenced blob {0} is missing")]
    Missing(BlobHash),
    #[error("referenced blob {0} failed integrity verification")]
    Unverified(BlobHash),
}

/// Require all hashes in a boundary record to resolve to integrity-checked trusted blobs.
///
/// # Errors
///
/// Returns the first missing or unverified reference.
pub fn validate_referenced_blobs(
    envelope: &Envelope,
    resolver: &impl BlobReferenceResolver,
) -> Result<(), ReferenceError> {
    for hash in envelope.referenced_hashes() {
        match resolver.status(hash) {
            BlobReferenceStatus::Missing => return Err(ReferenceError::Missing(hash.clone())),
            BlobReferenceStatus::Unverified => {
                return Err(ReferenceError::Unverified(hash.clone()));
            }
            BlobReferenceStatus::Verified => {}
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisorBinding {
    session_id: String,
    branch_id: String,
}

impl SupervisorBinding {
    /// Creates the trusted session and branch identity assigned to one agent stream.
    ///
    /// # Errors
    ///
    /// Returns a field error when either identifier is empty or exceeds the wire limit.
    pub fn new(
        session_id: impl Into<String>,
        branch_id: impl Into<String>,
    ) -> Result<Self, ProtocolError> {
        let binding = Self {
            session_id: session_id.into(),
            branch_id: branch_id.into(),
        };
        bounded_string(
            "binding.session_id",
            &binding.session_id,
            MAX_IDENTIFIER_BYTES,
        )?;
        bounded_string(
            "binding.branch_id",
            &binding.branch_id,
            MAX_IDENTIFIER_BYTES,
        )?;
        Ok(binding)
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StreamError {
    #[error("stream must begin with agent.start, received {kind}")]
    StartRequired { kind: String },
    #[error("stream contains more than one agent.start")]
    DuplicateStart,
    #[error("agent claimed session {received:?}; supervisor assigned {expected:?}")]
    SessionBindingMismatch { expected: String, received: String },
    #[error("agent claimed branch {received:?}; supervisor assigned {expected:?}")]
    BranchBindingMismatch { expected: String, received: String },
    #[error("sequence {received} is not greater than prior sequence {previous}")]
    NonMonotonicSequence { previous: u64, received: u64 },
    #[error("context revision {received} is not greater than prior revision {previous}")]
    NonMonotonicContextRevision { previous: u64, received: u64 },
    #[error("model request ID {request_id:?} was reused")]
    DuplicateModelRequest { request_id: String },
    #[error("model response has no pending request {request_id:?}")]
    ModelResponseWithoutRequest { request_id: String },
    #[error("tool call ID {call_id:?} was reused")]
    DuplicateToolCall { call_id: String },
    #[error("tool result has no pending call {call_id:?}")]
    ToolResultWithoutCall { call_id: String },
    #[error("safe point arrived before any context update")]
    SafePointWithoutContext,
    #[error("safe point context {received} differs from current context {expected}")]
    SafePointContextMismatch {
        expected: BlobHash,
        received: BlobHash,
    },
    #[error(
        "safe point has {model_requests} pending model requests and {tool_calls} pending tool calls"
    )]
    SafePointWithOutstanding {
        model_requests: usize,
        tool_calls: usize,
    },
    #[error(
        "completion has {model_requests} pending model requests and {tool_calls} pending tool calls"
    )]
    CompletionWithOutstanding {
        model_requests: usize,
        tool_calls: usize,
    },
    #[error("message {kind} arrived after agent.completion")]
    MessageAfterCompletion { kind: String },
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum IngestError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Reference(#[from] ReferenceError),
    #[error(transparent)]
    Stream(#[from] StreamError),
}

/// Stateful validation at the trusted supervisor's agent-stream boundary.
#[derive(Clone, Debug)]
pub struct StreamValidator {
    binding: SupervisorBinding,
    started: bool,
    completed: bool,
    last_sequence: Option<u64>,
    last_context_revision: Option<u64>,
    current_context_hash: Option<BlobHash>,
    seen_model_requests: HashSet<String>,
    pending_model_requests: HashSet<String>,
    seen_tool_calls: HashSet<String>,
    pending_tool_calls: HashSet<String>,
}

impl StreamValidator {
    #[must_use]
    pub fn new(binding: SupervisorBinding) -> Self {
        Self {
            binding,
            started: false,
            completed: false,
            last_sequence: None,
            last_context_revision: None,
            current_context_hash: None,
            seen_model_requests: HashSet::new(),
            pending_model_requests: HashSet::new(),
            seen_tool_calls: HashSet::new(),
            pending_tool_calls: HashSet::new(),
        }
    }

    /// Validate an integrity-checked boundary record before acknowledgment or persistence.
    ///
    /// # Errors
    ///
    /// Returns an ingest error when a blob is not verified or stream semantics are invalid.
    pub fn accept(
        &mut self,
        envelope: &Envelope,
        resolver: &impl BlobReferenceResolver,
    ) -> Result<(), IngestError> {
        validate(envelope)?;
        let mut candidate = self.clone();
        candidate.accept_stream(envelope)?;
        validate_referenced_blobs(envelope, resolver)?;
        *self = candidate;
        Ok(())
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.completed
    }

    fn accept_stream(&mut self, envelope: &Envelope) -> Result<(), StreamError> {
        if self.completed {
            return Err(StreamError::MessageAfterCompletion {
                kind: envelope.message.kind().to_owned(),
            });
        }

        if !self.started {
            let Message::AgentStart(start) = &envelope.message else {
                return Err(StreamError::StartRequired {
                    kind: envelope.message.kind().to_owned(),
                });
            };
            self.validate_binding(start)?;
            self.started = true;
            self.last_sequence = Some(envelope.sequence);
            return Ok(());
        }

        if matches!(envelope.message, Message::AgentStart(_)) {
            return Err(StreamError::DuplicateStart);
        }
        if let Some(previous) = self.last_sequence
            && envelope.sequence <= previous
        {
            return Err(StreamError::NonMonotonicSequence {
                previous,
                received: envelope.sequence,
            });
        }

        match &envelope.message {
            Message::AgentStart(_) => unreachable!("duplicate start handled above"),
            Message::ContextUpdate(update) => self.accept_context(update)?,
            Message::ModelRequest(request) => self.accept_model_request(request)?,
            Message::ModelResponse(response) => self.accept_model_response(response)?,
            Message::ToolCall(call) => self.accept_tool_call(call)?,
            Message::ToolResult(result) => self.accept_tool_result(result)?,
            Message::SafePoint(safe_point) => self.accept_safe_point(safe_point)?,
            Message::Completion(_) => self.accept_completion()?,
        }
        self.last_sequence = Some(envelope.sequence);
        Ok(())
    }

    fn validate_binding(&self, start: &AgentStart) -> Result<(), StreamError> {
        if start.session_id != self.binding.session_id {
            return Err(StreamError::SessionBindingMismatch {
                expected: self.binding.session_id.clone(),
                received: start.session_id.clone(),
            });
        }
        if start.branch_id != self.binding.branch_id {
            return Err(StreamError::BranchBindingMismatch {
                expected: self.binding.branch_id.clone(),
                received: start.branch_id.clone(),
            });
        }
        Ok(())
    }

    fn accept_context(&mut self, update: &ContextUpdate) -> Result<(), StreamError> {
        if let Some(previous) = self.last_context_revision
            && update.revision <= previous
        {
            return Err(StreamError::NonMonotonicContextRevision {
                previous,
                received: update.revision,
            });
        }
        self.last_context_revision = Some(update.revision);
        self.current_context_hash = Some(update.context_hash.clone());
        Ok(())
    }

    fn accept_model_request(&mut self, request: &ModelRequest) -> Result<(), StreamError> {
        if !self.seen_model_requests.insert(request.request_id.clone()) {
            return Err(StreamError::DuplicateModelRequest {
                request_id: request.request_id.clone(),
            });
        }
        self.pending_model_requests
            .insert(request.request_id.clone());
        Ok(())
    }

    fn accept_model_response(&mut self, response: &ModelResponse) -> Result<(), StreamError> {
        if !self.pending_model_requests.remove(&response.request_id) {
            return Err(StreamError::ModelResponseWithoutRequest {
                request_id: response.request_id.clone(),
            });
        }
        Ok(())
    }

    fn accept_tool_call(&mut self, call: &ToolCall) -> Result<(), StreamError> {
        if !self.seen_tool_calls.insert(call.call_id.clone()) {
            return Err(StreamError::DuplicateToolCall {
                call_id: call.call_id.clone(),
            });
        }
        self.pending_tool_calls.insert(call.call_id.clone());
        Ok(())
    }

    fn accept_tool_result(&mut self, result: &ToolResult) -> Result<(), StreamError> {
        if !self.pending_tool_calls.remove(&result.call_id) {
            return Err(StreamError::ToolResultWithoutCall {
                call_id: result.call_id.clone(),
            });
        }
        Ok(())
    }

    fn accept_safe_point(&self, safe_point: &SafePoint) -> Result<(), StreamError> {
        let Some(current) = &self.current_context_hash else {
            return Err(StreamError::SafePointWithoutContext);
        };
        if safe_point.context_hash != *current {
            return Err(StreamError::SafePointContextMismatch {
                expected: current.clone(),
                received: safe_point.context_hash.clone(),
            });
        }
        if !self.pending_model_requests.is_empty() || !self.pending_tool_calls.is_empty() {
            return Err(StreamError::SafePointWithOutstanding {
                model_requests: self.pending_model_requests.len(),
                tool_calls: self.pending_tool_calls.len(),
            });
        }
        Ok(())
    }

    fn accept_completion(&mut self) -> Result<(), StreamError> {
        if !self.pending_model_requests.is_empty() || !self.pending_tool_calls.is_empty() {
            return Err(StreamError::CompletionWithOutstanding {
                model_requests: self.pending_model_requests.len(),
                tool_calls: self.pending_tool_calls.len(),
            });
        }
        self.completed = true;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    AgentStart(AgentStart),
    ContextUpdate(ContextUpdate),
    ModelRequest(ModelRequest),
    ModelResponse(ModelResponse),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    SafePoint(SafePoint),
    Completion(Completion),
}

impl Message {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::AgentStart(_) => "agent.start",
            Self::ContextUpdate(_) => "context.update",
            Self::ModelRequest(_) => "model.request",
            Self::ModelResponse(_) => "model.response",
            Self::ToolCall(_) => "tool.call",
            Self::ToolResult(_) => "tool.result",
            Self::SafePoint(_) => "safe_point",
            Self::Completion(_) => "agent.completion",
        }
    }
}

macro_rules! payload {
    ($name:ident { $($field:ident: $type:ty),* $(,)? }) => {
        #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
        pub struct $name {
            $(pub $field: $type,)*
            #[serde(default, flatten)]
            pub extensions: Extensions,
        }
    };
}

payload!(AgentStart {
    agent_id: String,
    session_id: String,
    branch_id: String,
});
payload!(ContextUpdate {
    revision: u64,
    context_hash: BlobHash,
});
payload!(ModelRequest {
    request_id: String,
    model: String,
    input_hash: BlobHash,
});
payload!(ModelResponse {
    request_id: String,
    output_hash: BlobHash,
});
payload!(ToolCall {
    call_id: String,
    tool: String,
    input_hash: BlobHash,
});
payload!(ToolResult {
    call_id: String,
    outcome: ToolOutcome,
    output_hash: Option<BlobHash>,
});
payload!(SafePoint {
    safe_point_id: String,
    context_hash: BlobHash,
});
payload!(Completion {
    outcome: CompletionOutcome,
    output_hash: Option<BlobHash>,
});

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Succeeded,
    Failed,
    Denied,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProtocolError {
    #[error("JSONL record is empty")]
    EmptyLine,
    #[error("JSONL record is {actual} bytes; maximum is {maximum}")]
    LineTooLarge { actual: usize, maximum: usize },
    #[error("input contains more than one JSONL record")]
    MultipleRecords,
    #[error("input contains a bare carriage return")]
    BareCarriageReturn,
    #[error("JSON object contains duplicate key `{key}`")]
    DuplicateKey { key: String },
    #[error("malformed JSON at line {line}, column {column}: {message}")]
    MalformedJson {
        line: usize,
        column: usize,
        message: String,
    },
    #[error("missing required field `{field}`")]
    MissingField { field: String },
    #[error("field `{field}` is invalid: {reason}")]
    InvalidField { field: String, reason: String },
    #[error("protocol version {received} is unsupported; this runtime supports {supported}")]
    UnsupportedVersion { received: u16, supported: u16 },
    #[error("message type `{message_type}` is unknown")]
    UnknownMessageType { message_type: String },
    #[error("cannot encode message: {message}")]
    Encoding { message: String },
}

/// Decode exactly one bounded JSONL record.
///
/// # Errors
///
/// Returns a typed error when the record is malformed, too large, uses an unsupported version,
/// or contains a message that fails structural or semantic validation.
pub fn decode_line(input: &[u8]) -> Result<Envelope, ProtocolError> {
    if input.len() > MAX_JSONL_BYTES {
        return Err(ProtocolError::LineTooLarge {
            actual: input.len(),
            maximum: MAX_JSONL_BYTES,
        });
    }

    let record = strip_record_terminator(input);
    if record.iter().all(u8::is_ascii_whitespace) {
        return Err(ProtocolError::EmptyLine);
    }
    if record.contains(&b'\r') {
        return Err(ProtocolError::BareCarriageReturn);
    }
    if record.contains(&b'\n') {
        return Err(ProtocolError::MultipleRecords);
    }

    let value = unique_json::from_slice(record).map_err(|error| json_error(&error))?;
    let Value::Object(mut fields) = value else {
        return Err(ProtocolError::InvalidField {
            field: "$".to_owned(),
            reason: "envelope must be a JSON object".to_owned(),
        });
    };

    let protocol_version = required_u16(&mut fields, "protocol_version")?;
    if protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion {
            received: protocol_version,
            supported: CURRENT_PROTOCOL_VERSION,
        });
    }

    let sequence = required_u64(&mut fields, "sequence")?;
    let message_type = required_string(&mut fields, "type")?;
    let payload = fields
        .remove("payload")
        .ok_or_else(|| ProtocolError::MissingField {
            field: "payload".to_owned(),
        })?;
    if !payload.is_object() {
        return Err(ProtocolError::InvalidField {
            field: "payload".to_owned(),
            reason: "must be a JSON object".to_owned(),
        });
    }

    let message = decode_message(&message_type, payload)?;
    let envelope = Envelope {
        protocol_version,
        sequence,
        message,
        extensions: fields,
    };
    validate(&envelope)?;
    Ok(envelope)
}

/// Validate and encode one envelope, including its terminating newline.
///
/// # Errors
///
/// Returns a typed error when the envelope is invalid or its encoded record exceeds the input
/// boundary accepted by [`decode_line`].
pub fn encode_line(envelope: &Envelope) -> Result<String, ProtocolError> {
    validate(envelope)?;

    let mut fields = envelope.extensions.clone();
    fields.insert(
        "protocol_version".to_owned(),
        Value::from(envelope.protocol_version),
    );
    fields.insert("sequence".to_owned(), Value::from(envelope.sequence));
    fields.insert("type".to_owned(), Value::from(envelope.message.kind()));
    fields.insert("payload".to_owned(), encode_payload(&envelope.message)?);

    let mut encoded =
        serde_json::to_vec(&Value::Object(fields)).map_err(|error| ProtocolError::Encoding {
            message: error.to_string(),
        })?;
    encoded.push(b'\n');
    if encoded.len() > MAX_JSONL_BYTES {
        return Err(ProtocolError::LineTooLarge {
            actual: encoded.len(),
            maximum: MAX_JSONL_BYTES,
        });
    }

    String::from_utf8(encoded).map_err(|error| ProtocolError::Encoding {
        message: error.to_string(),
    })
}

fn strip_record_terminator(input: &[u8]) -> &[u8] {
    input
        .strip_suffix(b"\r\n")
        .or_else(|| input.strip_suffix(b"\n"))
        .unwrap_or(input)
}

fn json_error(error: &serde_json::Error) -> ProtocolError {
    let message = error.to_string();
    if let Some(remainder) = message
        .split_once(unique_json::DUPLICATE_KEY_PREFIX)
        .map(|(_, remainder)| remainder)
        && let Some((key, _)) = remainder.split_once('`')
    {
        return ProtocolError::DuplicateKey {
            key: key.to_owned(),
        };
    }
    ProtocolError::MalformedJson {
        line: error.line(),
        column: error.column(),
        message,
    }
}

fn required_value(fields: &mut Extensions, field: &str) -> Result<Value, ProtocolError> {
    fields
        .remove(field)
        .ok_or_else(|| ProtocolError::MissingField {
            field: field.to_owned(),
        })
}

fn required_u16(fields: &mut Extensions, field: &str) -> Result<u16, ProtocolError> {
    let value = required_value(fields, field)?;
    let value = value
        .as_u64()
        .and_then(|number| u16::try_from(number).ok())
        .ok_or_else(|| ProtocolError::InvalidField {
            field: field.to_owned(),
            reason: "must be an unsigned 16-bit integer".to_owned(),
        })?;
    Ok(value)
}

fn required_u64(fields: &mut Extensions, field: &str) -> Result<u64, ProtocolError> {
    let value = required_value(fields, field)?;
    value.as_u64().ok_or_else(|| ProtocolError::InvalidField {
        field: field.to_owned(),
        reason: "must be an unsigned integer".to_owned(),
    })
}

fn required_string(fields: &mut Extensions, field: &str) -> Result<String, ProtocolError> {
    let value = required_value(fields, field)?;
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| ProtocolError::InvalidField {
            field: field.to_owned(),
            reason: "must be a string".to_owned(),
        })
}

fn decode_message(message_type: &str, payload: Value) -> Result<Message, ProtocolError> {
    match message_type {
        "agent.start" => decode_payload(payload).map(Message::AgentStart),
        "context.update" => decode_payload(payload).map(Message::ContextUpdate),
        "model.request" => decode_payload(payload).map(Message::ModelRequest),
        "model.response" => decode_payload(payload).map(Message::ModelResponse),
        "tool.call" => decode_payload(payload).map(Message::ToolCall),
        "tool.result" => decode_payload(payload).map(Message::ToolResult),
        "safe_point" => decode_payload(payload).map(Message::SafePoint),
        "agent.completion" => decode_payload(payload).map(Message::Completion),
        _ => Err(ProtocolError::UnknownMessageType {
            message_type: message_type.to_owned(),
        }),
    }
}

fn decode_payload<T: DeserializeOwned>(payload: Value) -> Result<T, ProtocolError> {
    serde_json::from_value(payload).map_err(|error| payload_error(&error))
}

fn payload_error(error: &serde_json::Error) -> ProtocolError {
    let reason = error.to_string();
    let missing_prefix = "missing field `";
    if let Some(remainder) = reason.strip_prefix(missing_prefix)
        && let Some((field, _)) = remainder.split_once('`')
    {
        return ProtocolError::MissingField {
            field: format!("payload.{field}"),
        };
    }
    ProtocolError::InvalidField {
        field: "payload".to_owned(),
        reason,
    }
}

fn encode_payload(message: &Message) -> Result<Value, ProtocolError> {
    let result = match message {
        Message::AgentStart(payload) => serde_json::to_value(payload),
        Message::ContextUpdate(payload) => serde_json::to_value(payload),
        Message::ModelRequest(payload) => serde_json::to_value(payload),
        Message::ModelResponse(payload) => serde_json::to_value(payload),
        Message::ToolCall(payload) => serde_json::to_value(payload),
        Message::ToolResult(payload) => serde_json::to_value(payload),
        Message::SafePoint(payload) => serde_json::to_value(payload),
        Message::Completion(payload) => serde_json::to_value(payload),
    };
    result.map_err(|error| ProtocolError::Encoding {
        message: error.to_string(),
    })
}

fn validate(envelope: &Envelope) -> Result<(), ProtocolError> {
    if envelope.protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion {
            received: envelope.protocol_version,
            supported: CURRENT_PROTOCOL_VERSION,
        });
    }
    bounded_integer("sequence", envelope.sequence, MAX_SEQUENCE)?;
    reject_reserved(
        &envelope.extensions,
        &["protocol_version", "sequence", "type", "payload"],
        "$",
    )?;

    match &envelope.message {
        Message::AgentStart(payload) => {
            bounded_string("payload.agent_id", &payload.agent_id, MAX_IDENTIFIER_BYTES)?;
            bounded_string(
                "payload.session_id",
                &payload.session_id,
                MAX_IDENTIFIER_BYTES,
            )?;
            bounded_string(
                "payload.branch_id",
                &payload.branch_id,
                MAX_IDENTIFIER_BYTES,
            )?;
            reject_reserved(
                &payload.extensions,
                &["agent_id", "session_id", "branch_id"],
                "payload",
            )
        }
        Message::ContextUpdate(payload) => {
            bounded_integer("payload.revision", payload.revision, MAX_CONTEXT_REVISION)?;
            reject_reserved(
                &payload.extensions,
                &["revision", "context_hash"],
                "payload",
            )
        }
        Message::ModelRequest(payload) => {
            bounded_string(
                "payload.request_id",
                &payload.request_id,
                MAX_IDENTIFIER_BYTES,
            )?;
            bounded_string("payload.model", &payload.model, MAX_NAME_BYTES)?;
            reject_reserved(
                &payload.extensions,
                &["request_id", "model", "input_hash"],
                "payload",
            )
        }
        Message::ModelResponse(payload) => {
            bounded_string(
                "payload.request_id",
                &payload.request_id,
                MAX_IDENTIFIER_BYTES,
            )?;
            reject_reserved(
                &payload.extensions,
                &["request_id", "output_hash"],
                "payload",
            )
        }
        Message::ToolCall(payload) => {
            bounded_string("payload.call_id", &payload.call_id, MAX_IDENTIFIER_BYTES)?;
            bounded_string("payload.tool", &payload.tool, MAX_NAME_BYTES)?;
            reject_reserved(
                &payload.extensions,
                &["call_id", "tool", "input_hash"],
                "payload",
            )
        }
        Message::ToolResult(payload) => {
            bounded_string("payload.call_id", &payload.call_id, MAX_IDENTIFIER_BYTES)?;
            reject_reserved(
                &payload.extensions,
                &["call_id", "outcome", "output_hash"],
                "payload",
            )
        }
        Message::SafePoint(payload) => {
            bounded_string(
                "payload.safe_point_id",
                &payload.safe_point_id,
                MAX_IDENTIFIER_BYTES,
            )?;
            reject_reserved(
                &payload.extensions,
                &["safe_point_id", "context_hash"],
                "payload",
            )
        }
        Message::Completion(payload) => {
            reject_reserved(&payload.extensions, &["outcome", "output_hash"], "payload")
        }
    }
}

fn bounded_string(field: &str, value: &str, maximum: usize) -> Result<(), ProtocolError> {
    if value.trim().is_empty() {
        return Err(ProtocolError::InvalidField {
            field: field.to_owned(),
            reason: "must not be empty".to_owned(),
        });
    }
    if value.len() > maximum {
        return Err(ProtocolError::InvalidField {
            field: field.to_owned(),
            reason: format!("must be at most {maximum} bytes"),
        });
    }
    Ok(())
}

fn bounded_integer(field: &str, value: u64, maximum: u64) -> Result<(), ProtocolError> {
    if value > maximum {
        return Err(ProtocolError::InvalidField {
            field: field.to_owned(),
            reason: format!("must be at most {maximum}"),
        });
    }
    Ok(())
}

fn reject_reserved(
    extensions: &Extensions,
    reserved: &[&str],
    scope: &str,
) -> Result<(), ProtocolError> {
    if let Some(field) = reserved
        .iter()
        .find(|field| extensions.contains_key(**field))
    {
        return Err(ProtocolError::InvalidField {
            field: format!("{scope}.{field}"),
            reason: "extension collides with a protocol field".to_owned(),
        });
    }
    Ok(())
}
