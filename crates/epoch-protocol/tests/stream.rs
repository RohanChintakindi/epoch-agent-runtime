use epoch_blob::BlobHash;
use epoch_protocol::{
    AgentStart, BlobReferenceResolver, BlobReferenceStatus, Completion, CompletionOutcome,
    ContextUpdate, Envelope, Extensions, IngestError, Message, ModelRequest, ModelResponse,
    SafePoint, StreamError, StreamValidator, SupervisorBinding, ToolCall, ToolOutcome, ToolResult,
};

struct AllVerified;

impl BlobReferenceResolver for AllVerified {
    fn status(&self, _hash: &BlobHash) -> BlobReferenceStatus {
        BlobReferenceStatus::Verified
    }
}

struct NoneVerified;

impl BlobReferenceResolver for NoneVerified {
    fn status(&self, _hash: &BlobHash) -> BlobReferenceStatus {
        BlobReferenceStatus::Missing
    }
}

fn hash(label: &str) -> BlobHash {
    BlobHash::digest(label.as_bytes())
}

fn envelope(sequence: u64, message: Message) -> Envelope {
    Envelope::new(sequence, message)
}

#[test]
fn claims_only_ingest_validates_stream_without_claiming_blob_verification() {
    let binding = SupervisorBinding::new("session", "branch").expect("valid binding");
    let mut validator = StreamValidator::new(binding);
    validator
        .accept_claims(&start(0, "session", "branch"))
        .expect("bound start is accepted");
    validator
        .accept_claims(&context(1, 0, hash("not-uploaded")))
        .expect("untrusted hash claim does not require a trusted blob");

    let wrong_binding = SupervisorBinding::new("session", "other").expect("valid binding");
    let mut wrong = StreamValidator::new(wrong_binding);
    assert!(matches!(
        wrong.accept_claims(&start(0, "session", "branch")),
        Err(IngestError::Stream(
            StreamError::BranchBindingMismatch { .. }
        ))
    ));
}

fn start(sequence: u64, session_id: &str, branch_id: &str) -> Envelope {
    envelope(
        sequence,
        Message::AgentStart(AgentStart {
            agent_id: "agent".to_owned(),
            session_id: session_id.to_owned(),
            branch_id: branch_id.to_owned(),
            extensions: Extensions::new(),
        }),
    )
}

fn context(sequence: u64, revision: u64, context_hash: BlobHash) -> Envelope {
    envelope(
        sequence,
        Message::ContextUpdate(ContextUpdate {
            revision,
            context_hash,
            extensions: Extensions::new(),
        }),
    )
}

fn model_request(sequence: u64, request_id: &str) -> Envelope {
    envelope(
        sequence,
        Message::ModelRequest(ModelRequest {
            request_id: request_id.to_owned(),
            model: "recorded-model".to_owned(),
            input_hash: hash(&format!("model-input:{request_id}")),
            extensions: Extensions::new(),
        }),
    )
}

fn model_response(sequence: u64, request_id: &str) -> Envelope {
    envelope(
        sequence,
        Message::ModelResponse(ModelResponse {
            request_id: request_id.to_owned(),
            output_hash: hash(&format!("model-output:{request_id}")),
            extensions: Extensions::new(),
        }),
    )
}

fn tool_call(sequence: u64, call_id: &str) -> Envelope {
    envelope(
        sequence,
        Message::ToolCall(ToolCall {
            call_id: call_id.to_owned(),
            tool: "fixture.tool".to_owned(),
            input_hash: hash(&format!("tool-input:{call_id}")),
            extensions: Extensions::new(),
        }),
    )
}

fn tool_result(sequence: u64, call_id: &str) -> Envelope {
    envelope(
        sequence,
        Message::ToolResult(ToolResult {
            call_id: call_id.to_owned(),
            outcome: ToolOutcome::Succeeded,
            output_hash: Some(hash(&format!("tool-output:{call_id}"))),
            extensions: Extensions::new(),
        }),
    )
}

fn safe_point(sequence: u64, context_hash: BlobHash) -> Envelope {
    envelope(
        sequence,
        Message::SafePoint(SafePoint {
            safe_point_id: format!("safe-{sequence}"),
            context_hash,
            extensions: Extensions::new(),
        }),
    )
}

fn completion(sequence: u64) -> Envelope {
    envelope(
        sequence,
        Message::Completion(Completion {
            outcome: CompletionOutcome::Succeeded,
            output_hash: Some(hash("completion")),
            extensions: Extensions::new(),
        }),
    )
}

fn validator() -> StreamValidator {
    StreamValidator::new(SupervisorBinding::new("session", "branch").expect("valid binding"))
}

#[test]
fn accepts_a_coherent_supervisor_bound_stream() {
    let current = hash("context");
    let records = [
        start(0, "session", "branch"),
        context(1, 0, current.clone()),
        model_request(2, "request"),
        model_response(3, "request"),
        tool_call(4, "call"),
        tool_result(5, "call"),
        safe_point(6, current),
        completion(7),
    ];
    let mut validator = validator();

    for record in records {
        validator
            .accept(&record, &AllVerified)
            .unwrap_or_else(|error| {
                panic!("{} should be accepted: {error}", record.message.kind())
            });
    }
    assert!(validator.is_complete());
}

#[test]
fn requires_exactly_one_start_and_supervisor_binding() {
    let mut validator = validator();
    assert_eq!(
        validator.accept(&context(0, 0, hash("context")), &AllVerified),
        Err(IngestError::Stream(StreamError::StartRequired {
            kind: "context.update".to_owned(),
        }))
    );
    assert_eq!(
        validator.accept(&start(0, "wrong", "branch"), &AllVerified),
        Err(IngestError::Stream(StreamError::SessionBindingMismatch {
            expected: "session".to_owned(),
            received: "wrong".to_owned(),
        }))
    );
    assert_eq!(
        validator.accept(&start(0, "session", "wrong"), &AllVerified),
        Err(IngestError::Stream(StreamError::BranchBindingMismatch {
            expected: "branch".to_owned(),
            received: "wrong".to_owned(),
        }))
    );

    validator
        .accept(&start(0, "session", "branch"), &AllVerified)
        .expect("corrected start should be accepted after rejected attempts");
    assert_eq!(
        validator.accept(&start(1, "session", "branch"), &AllVerified),
        Err(IngestError::Stream(StreamError::DuplicateStart))
    );
}

#[test]
fn enforces_strictly_monotonic_sequences_and_context_revisions_atomically() {
    let mut validator = validator();
    validator
        .accept(&start(5, "session", "branch"), &AllVerified)
        .expect("start");
    validator
        .accept(&context(6, 9, hash("nine")), &AllVerified)
        .expect("first context");

    assert_eq!(
        validator.accept(&context(6, 10, hash("ten")), &AllVerified),
        Err(IngestError::Stream(StreamError::NonMonotonicSequence {
            previous: 6,
            received: 6,
        }))
    );
    assert_eq!(
        validator.accept(&context(7, 9, hash("nine-again")), &AllVerified),
        Err(IngestError::Stream(
            StreamError::NonMonotonicContextRevision {
                previous: 9,
                received: 9,
            }
        ))
    );
    validator
        .accept(&context(7, 10, hash("ten")), &AllVerified)
        .expect("rejected records must not advance sequence or revision");
}

#[test]
fn correlates_model_requests_and_tool_calls_without_id_reuse() {
    let mut validator = validator();
    validator
        .accept(&start(0, "session", "branch"), &AllVerified)
        .expect("start");
    validator
        .accept(&model_request(1, "request"), &AllVerified)
        .expect("request");
    assert!(matches!(
        validator.accept(&model_request(2, "request"), &AllVerified),
        Err(IngestError::Stream(
            StreamError::DuplicateModelRequest { .. }
        ))
    ));
    assert!(matches!(
        validator.accept(&model_response(2, "unknown"), &AllVerified),
        Err(IngestError::Stream(
            StreamError::ModelResponseWithoutRequest { .. }
        ))
    ));
    validator
        .accept(&model_response(2, "request"), &AllVerified)
        .expect("correlated response");

    validator
        .accept(&tool_call(3, "call"), &AllVerified)
        .expect("tool call");
    assert!(matches!(
        validator.accept(&tool_call(4, "call"), &AllVerified),
        Err(IngestError::Stream(StreamError::DuplicateToolCall { .. }))
    ));
    assert!(matches!(
        validator.accept(&tool_result(4, "unknown"), &AllVerified),
        Err(IngestError::Stream(
            StreamError::ToolResultWithoutCall { .. }
        ))
    ));
    validator
        .accept(&tool_result(4, "call"), &AllVerified)
        .expect("correlated result");
}

#[test]
fn safe_points_require_current_context_and_no_outstanding_operations() {
    let mut validator = validator();
    validator
        .accept(&start(0, "session", "branch"), &AllVerified)
        .expect("start");
    assert_eq!(
        validator.accept(&safe_point(1, hash("missing")), &AllVerified),
        Err(IngestError::Stream(StreamError::SafePointWithoutContext))
    );

    let current = hash("current");
    validator
        .accept(&context(1, 0, current.clone()), &AllVerified)
        .expect("context");
    assert!(matches!(
        validator.accept(&safe_point(2, hash("wrong")), &AllVerified),
        Err(IngestError::Stream(
            StreamError::SafePointContextMismatch { .. }
        ))
    ));
    validator
        .accept(&model_request(2, "request"), &AllVerified)
        .expect("request");
    assert_eq!(
        validator.accept(&safe_point(3, current.clone()), &AllVerified),
        Err(IngestError::Stream(StreamError::SafePointWithOutstanding {
            model_requests: 1,
            tool_calls: 0,
        }))
    );
    validator
        .accept(&model_response(3, "request"), &AllVerified)
        .expect("response");
    validator
        .accept(&safe_point(4, current), &AllVerified)
        .expect("coherent safe point");
}

#[test]
fn rejects_completion_with_pending_work_and_every_message_after_completion() {
    let mut validator = validator();
    validator
        .accept(&start(0, "session", "branch"), &AllVerified)
        .expect("start");
    validator
        .accept(&tool_call(1, "call"), &AllVerified)
        .expect("call");
    assert_eq!(
        validator.accept(&completion(2), &AllVerified),
        Err(IngestError::Stream(
            StreamError::CompletionWithOutstanding {
                model_requests: 0,
                tool_calls: 1,
            }
        ))
    );
    validator
        .accept(&tool_result(2, "call"), &AllVerified)
        .expect("result");
    validator
        .accept(&completion(3), &AllVerified)
        .expect("completion");
    assert_eq!(
        validator.accept(&context(4, 0, hash("late")), &AllVerified),
        Err(IngestError::Stream(StreamError::MessageAfterCompletion {
            kind: "context.update".to_owned(),
        }))
    );
}

#[test]
fn missing_blob_rejection_does_not_advance_stream_state() {
    let mut validator = validator();
    validator
        .accept(&start(0, "session", "branch"), &NoneVerified)
        .expect("start has no references");
    let request = model_request(1, "request");
    assert!(matches!(
        validator.accept(&request, &NoneVerified),
        Err(IngestError::Reference(_))
    ));
    validator
        .accept(&request, &AllVerified)
        .expect("verified retry should retain original sequence");
}
