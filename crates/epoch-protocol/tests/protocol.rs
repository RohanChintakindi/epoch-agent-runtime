use epoch_protocol::{
    CURRENT_PROTOCOL_VERSION, Envelope, MAX_JSONL_BYTES, Message, ProtocolError, ToolOutcome,
    decode_line, encode_line,
};
use serde_json::json;

fn record(message_type: &str, payload: serde_json::Value) -> Vec<u8> {
    let mut fields = serde_json::Map::new();
    fields.insert(
        "protocol_version".to_owned(),
        json!(CURRENT_PROTOCOL_VERSION),
    );
    fields.insert("sequence".to_owned(), json!(7));
    fields.insert("type".to_owned(), json!(message_type));
    fields.insert("payload".to_owned(), payload);
    serde_json::to_vec(&serde_json::Value::Object(fields)).expect("test record should serialize")
}

#[test]
fn decodes_every_v1_boundary_message() {
    let cases = [
        (
            "agent.start",
            json!({"agent_id":"agent-1","session_id":"ses-1","branch_id":"br-1"}),
        ),
        (
            "context.update",
            json!({"revision":2,"context_hash":"sha256:context"}),
        ),
        (
            "model.request",
            json!({"request_id":"req-1","model":"recorded-v1","input_hash":"sha256:in"}),
        ),
        (
            "model.response",
            json!({"request_id":"req-1","output_hash":"sha256:out"}),
        ),
        (
            "tool.call",
            json!({"call_id":"call-1","tool":"write_file","input_hash":"sha256:args"}),
        ),
        (
            "tool.result",
            json!({"call_id":"call-1","outcome":"succeeded","output_hash":"sha256:result"}),
        ),
        (
            "safe_point",
            json!({"safe_point_id":"safe-1","context_hash":"sha256:context"}),
        ),
        (
            "agent.completion",
            json!({"outcome":"succeeded","output_hash":"sha256:final"}),
        ),
    ];

    for (message_type, payload) in cases {
        let decoded = decode_line(&record(message_type, payload))
            .unwrap_or_else(|error| panic!("{message_type} should decode: {error}"));
        assert_eq!(decoded.protocol_version, CURRENT_PROTOCOL_VERSION);
        assert_eq!(decoded.sequence, 7);
        assert_eq!(decoded.message.kind(), message_type);
    }
}

#[test]
fn exposes_typed_message_payloads() {
    let decoded = decode_line(&record(
        "tool.result",
        json!({"call_id":"call-9","outcome":"denied","output_hash":null}),
    ))
    .expect("valid tool result should decode");

    let Message::ToolResult(result) = decoded.message else {
        panic!("expected a typed tool result")
    };
    assert_eq!(result.call_id, "call-9");
    assert_eq!(result.outcome, ToolOutcome::Denied);
    assert_eq!(result.output_hash, None);
}

#[test]
fn rejects_unsupported_protocol_versions_before_dispatch() {
    let input = br#"{"protocol_version":2,"sequence":0,"type":"agent.start","payload":{}}"#;
    assert_eq!(
        decode_line(input),
        Err(ProtocolError::UnsupportedVersion {
            received: 2,
            supported: CURRENT_PROTOCOL_VERSION,
        })
    );
}

#[test]
fn preserves_unknown_fields_for_forward_compatible_round_trips() {
    let input = br#"{"protocol_version":1,"sequence":8,"type":"context.update","payload":{"revision":3,"context_hash":"sha256:c","future_hint":{"mode":"x"}},"trace_id":"trace-1"}"#;
    let decoded = decode_line(input).expect("unknown fields should not reject a known v1 message");

    assert_eq!(decoded.extensions["trace_id"], json!("trace-1"));
    let Message::ContextUpdate(update) = &decoded.message else {
        panic!("expected context update")
    };
    assert_eq!(update.extensions["future_hint"], json!({"mode":"x"}));

    let encoded = encode_line(&decoded).expect("known message with extensions should encode");
    let reparsed = decode_line(encoded.as_bytes()).expect("encoded record should decode");
    assert_eq!(reparsed, decoded);
}

#[test]
fn returns_typed_errors_for_input_boundaries() {
    let mut lf = record(
        "context.update",
        json!({"revision":1,"context_hash":"sha256:c"}),
    );
    lf.push(b'\n');
    assert!(decode_line(&lf).is_ok());
    let mut crlf = lf;
    crlf.insert(crlf.len() - 1, b'\r');
    assert!(decode_line(&crlf).is_ok());

    assert_eq!(decode_line(b"\n"), Err(ProtocolError::EmptyLine));
    assert!(matches!(
        decode_line(b"{"),
        Err(ProtocolError::MalformedJson { .. })
    ));
    assert_eq!(decode_line(b"{}\n{}"), Err(ProtocolError::MultipleRecords));

    let oversized = vec![b' '; MAX_JSONL_BYTES + 1];
    assert_eq!(
        decode_line(&oversized),
        Err(ProtocolError::LineTooLarge {
            actual: MAX_JSONL_BYTES + 1,
            maximum: MAX_JSONL_BYTES,
        })
    );
}

#[test]
fn returns_typed_errors_for_unknown_types_and_invalid_payloads() {
    assert_eq!(
        decode_line(&record("future.event", json!({}))),
        Err(ProtocolError::UnknownMessageType {
            message_type: "future.event".to_owned(),
        })
    );

    assert_eq!(
        decode_line(&record(
            "agent.start",
            json!({"agent_id":"agent-1", "session_id":"ses-1"}),
        )),
        Err(ProtocolError::MissingField {
            field: "payload.branch_id".to_owned(),
        })
    );

    assert_eq!(
        decode_line(&record(
            "agent.start",
            json!({"agent_id":"", "session_id":"ses-1", "branch_id":"br-1"}),
        )),
        Err(ProtocolError::InvalidField {
            field: "payload.agent_id".to_owned(),
            reason: "must not be empty".to_owned(),
        })
    );
}

#[test]
fn encoder_emits_exactly_one_newline_terminated_record() {
    let decoded = decode_line(&record(
        "safe_point",
        json!({"safe_point_id":"safe-1","context_hash":"sha256:ctx"}),
    ))
    .expect("fixture should decode");
    let encoded = encode_line(&decoded).expect("fixture should encode");

    assert!(encoded.ends_with('\n'));
    assert_eq!(encoded.matches('\n').count(), 1);
    assert!(encoded.len() <= MAX_JSONL_BYTES);
}

#[test]
fn encoder_validates_programmatically_constructed_envelopes() {
    let mut decoded = decode_line(&record(
        "model.request",
        json!({"request_id":"req-1","model":"recorded-v1","input_hash":"sha256:in"}),
    ))
    .expect("fixture should decode");
    let Message::ModelRequest(request) = &mut decoded.message else {
        panic!("expected model request")
    };
    request.model.clear();

    assert_eq!(
        encode_line(&Envelope { ..decoded }),
        Err(ProtocolError::InvalidField {
            field: "payload.model".to_owned(),
            reason: "must not be empty".to_owned(),
        })
    );
}
