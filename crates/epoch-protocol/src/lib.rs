//! Versioned JSONL messages exchanged between an agent and the Epoch supervisor.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use thiserror::Error;

pub const CURRENT_PROTOCOL_VERSION: u16 = 1;
pub const MAX_JSONL_BYTES: usize = 1024 * 1024;

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
    context_hash: String,
});
payload!(ModelRequest {
    request_id: String,
    model: String,
    input_hash: String,
});
payload!(ModelResponse {
    request_id: String,
    output_hash: String,
});
payload!(ToolCall {
    call_id: String,
    tool: String,
    input_hash: String,
});
payload!(ToolResult {
    call_id: String,
    outcome: ToolOutcome,
    output_hash: Option<String>,
});
payload!(SafePoint {
    safe_point_id: String,
    context_hash: String,
});
payload!(Completion {
    outcome: CompletionOutcome,
    output_hash: Option<String>,
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

#[derive(Clone, Debug, Error, PartialEq)]
pub enum ProtocolError {
    #[error("JSONL record is empty")]
    EmptyLine,
    #[error("JSONL record is {actual} bytes; maximum is {maximum}")]
    LineTooLarge { actual: usize, maximum: usize },
    #[error("input contains more than one JSONL record")]
    MultipleRecords,
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
    if record.contains(&b'\n') || record.contains(&b'\r') {
        return Err(ProtocolError::MultipleRecords);
    }

    let value: Value = serde_json::from_slice(record).map_err(|error| json_error(&error))?;
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
    let without_newline = input.strip_suffix(b"\n").unwrap_or(input);
    without_newline
        .strip_suffix(b"\r")
        .unwrap_or(without_newline)
}

fn json_error(error: &serde_json::Error) -> ProtocolError {
    ProtocolError::MalformedJson {
        line: error.line(),
        column: error.column(),
        message: error.to_string(),
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
    reject_reserved(
        &envelope.extensions,
        &["protocol_version", "sequence", "type", "payload"],
        "$",
    )?;

    match &envelope.message {
        Message::AgentStart(payload) => {
            nonempty("payload.agent_id", &payload.agent_id)?;
            nonempty("payload.session_id", &payload.session_id)?;
            nonempty("payload.branch_id", &payload.branch_id)?;
            reject_reserved(
                &payload.extensions,
                &["agent_id", "session_id", "branch_id"],
                "payload",
            )
        }
        Message::ContextUpdate(payload) => {
            nonempty("payload.context_hash", &payload.context_hash)?;
            reject_reserved(
                &payload.extensions,
                &["revision", "context_hash"],
                "payload",
            )
        }
        Message::ModelRequest(payload) => {
            nonempty("payload.request_id", &payload.request_id)?;
            nonempty("payload.model", &payload.model)?;
            nonempty("payload.input_hash", &payload.input_hash)?;
            reject_reserved(
                &payload.extensions,
                &["request_id", "model", "input_hash"],
                "payload",
            )
        }
        Message::ModelResponse(payload) => {
            nonempty("payload.request_id", &payload.request_id)?;
            nonempty("payload.output_hash", &payload.output_hash)?;
            reject_reserved(
                &payload.extensions,
                &["request_id", "output_hash"],
                "payload",
            )
        }
        Message::ToolCall(payload) => {
            nonempty("payload.call_id", &payload.call_id)?;
            nonempty("payload.tool", &payload.tool)?;
            nonempty("payload.input_hash", &payload.input_hash)?;
            reject_reserved(
                &payload.extensions,
                &["call_id", "tool", "input_hash"],
                "payload",
            )
        }
        Message::ToolResult(payload) => {
            nonempty("payload.call_id", &payload.call_id)?;
            optional_nonempty("payload.output_hash", payload.output_hash.as_deref())?;
            reject_reserved(
                &payload.extensions,
                &["call_id", "outcome", "output_hash"],
                "payload",
            )
        }
        Message::SafePoint(payload) => {
            nonempty("payload.safe_point_id", &payload.safe_point_id)?;
            nonempty("payload.context_hash", &payload.context_hash)?;
            reject_reserved(
                &payload.extensions,
                &["safe_point_id", "context_hash"],
                "payload",
            )
        }
        Message::Completion(payload) => {
            optional_nonempty("payload.output_hash", payload.output_hash.as_deref())?;
            reject_reserved(&payload.extensions, &["outcome", "output_hash"], "payload")
        }
    }
}

fn nonempty(field: &str, value: &str) -> Result<(), ProtocolError> {
    if value.trim().is_empty() {
        return Err(ProtocolError::InvalidField {
            field: field.to_owned(),
            reason: "must not be empty".to_owned(),
        });
    }
    Ok(())
}

fn optional_nonempty(field: &str, value: Option<&str>) -> Result<(), ProtocolError> {
    if let Some(value) = value {
        nonempty(field, value)?;
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
