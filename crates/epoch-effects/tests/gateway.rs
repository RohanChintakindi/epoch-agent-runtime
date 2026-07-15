use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use epoch_core::{BranchId, SessionId};
use epoch_effects::{
    AttemptState, AuthorizationDecision, AuthorizationRequest, Authorizer, CanonicalIntent,
    DeterministicLocalDispatcher, DispatchFailureCode, DispatchOutcome, DispatchRequest,
    DispatchResult, EffectDispatcher, EffectGateway, EffectState, FaultPoint, FaultSafety,
    GatewayError,
};
use epoch_storage::Store;
use rusqlite::params;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    database: PathBuf,
    blobs: PathBuf,
    session: SessionId,
    branch: BranchId,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().expect("temporary runtime");
        let database = directory.path().join("state.db");
        let blobs = directory.path().join("blobs");
        let session = SessionId::new();
        let branch = BranchId::new();
        let store = Store::open(&database).expect("open store");
        store
            .connection()
            .execute(
                "INSERT INTO sessions (id, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, 'running', 0, 0)",
                [session.to_string()],
            )
            .expect("session");
        store
            .connection()
            .execute(
                "INSERT INTO branches \
                 (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
                 VALUES (?1, ?2, 'running', 0, 0)",
                params![branch.to_string(), session.to_string()],
            )
            .expect("branch");
        drop(store);
        Self {
            _directory: directory,
            database,
            blobs,
            session,
            branch,
        }
    }

    fn intent(&self, arguments: Value) -> CanonicalIntent {
        CanonicalIntent::new(
            self.session,
            self.branch,
            "turn-7/email-1",
            "email.send",
            "mailbox:demo",
            arguments,
            3,
        )
        .expect("valid intent")
    }

    fn gateway(
        &self,
        authorizer: Arc<dyn Authorizer>,
        dispatcher: Arc<dyn EffectDispatcher>,
    ) -> EffectGateway {
        EffectGateway::open(&self.database, &self.blobs, authorizer, dispatcher)
            .expect("open gateway")
    }
}

#[derive(Default)]
struct AllowAuthorizer {
    calls: AtomicUsize,
}

impl Authorizer for AllowAuthorizer {
    fn authorize(&self, _request: &AuthorizationRequest<'_>) -> AuthorizationDecision {
        self.calls.fetch_add(1, Ordering::SeqCst);
        AuthorizationDecision::Allow
    }
}

#[derive(Default)]
struct DenyAuthorizer;

impl Authorizer for DenyAuthorizer {
    fn authorize(&self, _request: &AuthorizationRequest<'_>) -> AuthorizationDecision {
        AuthorizationDecision::Deny
    }
}

#[derive(Default)]
struct IdempotentDispatcher {
    calls: AtomicUsize,
    committed: Mutex<HashMap<String, Vec<u8>>>,
}

impl EffectDispatcher for IdempotentDispatcher {
    fn dispatch(&self, request: &DispatchRequest<'_>) -> DispatchOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut committed = self.committed.lock().expect("dispatcher lock");
        let bytes = committed
            .entry(request.operation_id().to_string())
            .or_insert_with(|| {
                serde_json::to_vec(&json!({
                    "accepted": true,
                    "operation_id": request.operation_id(),
                }))
                .expect("fixture JSON")
            })
            .clone();
        DispatchOutcome::Committed(DispatchResult {
            bytes,
            media_type: "application/json".to_owned(),
            downstream_reference: Some(format!("mock:{}", request.operation_id())),
        })
    }
}

#[test]
fn canonical_intent_and_operation_id_are_stable_across_object_insertion_order() {
    let fixture = Fixture::new();
    let left = fixture.intent(json!({"to": "a@example.com", "body": {"b": 2, "a": 1}}));

    let mut nested = serde_json::Map::new();
    nested.insert("a".to_owned(), json!(1));
    nested.insert("b".to_owned(), json!(2));
    let mut args = serde_json::Map::new();
    args.insert("body".to_owned(), Value::Object(nested));
    args.insert("to".to_owned(), json!("a@example.com"));
    let right = fixture.intent(Value::Object(args));

    assert_eq!(left.operation_id(), right.operation_id());
    assert_eq!(left.input_hash(), right.input_hash());
    assert_eq!(left.canonical_bytes(), right.canonical_bytes());
}

#[test]
fn denial_is_durable_and_dispatch_is_never_called() {
    let fixture = Fixture::new();
    let dispatcher = Arc::new(IdempotentDispatcher::default());
    let gateway = fixture.gateway(Arc::new(DenyAuthorizer), dispatcher.clone());
    let intent = fixture.intent(json!({"to": "a@example.com"}));

    assert!(matches!(
        gateway.execute(&intent, FaultPoint::None),
        Err(GatewayError::AuthorizationDenied { .. })
    ));
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        gateway
            .inspect(intent.operation_id())
            .expect("inspect")
            .state,
        EffectState::Failed
    );
    assert_eq!(
        gateway
            .history(intent.operation_id())
            .expect("history")
            .into_iter()
            .map(|transition| transition.state)
            .collect::<Vec<_>>(),
        [EffectState::Requested, EffectState::Failed]
    );
}

#[test]
fn committed_duplicate_replays_exact_recorded_bytes_without_authorize_or_dispatch() {
    let fixture = Fixture::new();
    let authorizer = Arc::new(AllowAuthorizer::default());
    let dispatcher = Arc::new(IdempotentDispatcher::default());
    let gateway = fixture.gateway(authorizer.clone(), dispatcher.clone());
    let intent = fixture.intent(json!({"to": "a@example.com", "body": "hello"}));

    let first = gateway
        .execute(&intent, FaultPoint::None)
        .expect("first dispatch");
    let replay = gateway
        .execute(&intent, FaultPoint::None)
        .expect("committed replay");

    assert!(!first.replayed);
    assert!(replay.replayed);
    assert_eq!(first.result, replay.result);
    assert_eq!(first.result_hash, replay.result_hash);
    assert_eq!(authorizer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        gateway
            .history(intent.operation_id())
            .expect("history")
            .into_iter()
            .map(|transition| transition.state)
            .collect::<Vec<_>>(),
        [
            EffectState::Requested,
            EffectState::Prepared,
            EffectState::Dispatched,
            EffectState::Committed,
        ]
    );
}

#[test]
fn same_operation_id_with_materially_different_input_fails_closed() {
    let fixture = Fixture::new();
    let dispatcher = Arc::new(IdempotentDispatcher::default());
    let gateway = fixture.gateway(Arc::new(AllowAuthorizer::default()), dispatcher.clone());
    let first = fixture.intent(json!({"body": "one"}));
    let changed = fixture.intent(json!({"body": "two"}));
    assert_eq!(first.operation_id(), changed.operation_id());

    gateway
        .execute(&first, FaultPoint::None)
        .expect("first dispatch");
    assert!(matches!(
        gateway.execute(&changed, FaultPoint::None),
        Err(GatewayError::OperationInputConflict { .. })
    ));
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
}

struct BlockingDispatcher {
    calls: AtomicUsize,
    entered: Barrier,
    release: Barrier,
}

impl EffectDispatcher for BlockingDispatcher {
    fn dispatch(&self, _request: &DispatchRequest<'_>) -> DispatchOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.wait();
        self.release.wait();
        DispatchOutcome::Committed(DispatchResult {
            bytes: b"done".to_vec(),
            media_type: "text/plain".to_owned(),
            downstream_reference: None,
        })
    }
}

#[test]
fn concurrent_duplicate_is_suppressed_while_first_dispatch_is_in_flight() {
    let fixture = Fixture::new();
    let dispatcher = Arc::new(BlockingDispatcher {
        calls: AtomicUsize::new(0),
        entered: Barrier::new(2),
        release: Barrier::new(2),
    });
    let gateway =
        Arc::new(fixture.gateway(Arc::new(AllowAuthorizer::default()), dispatcher.clone()));
    let intent = Arc::new(fixture.intent(json!({"body": "once"})));

    let worker_gateway = Arc::clone(&gateway);
    let worker_intent = Arc::clone(&intent);
    let worker = thread::spawn(move || worker_gateway.execute(&worker_intent, FaultPoint::None));
    dispatcher.entered.wait();

    assert!(matches!(
        gateway.execute(&intent, FaultPoint::None),
        Err(GatewayError::UnresolvedOperation {
            state: EffectState::Dispatched,
            ..
        })
    ));
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
    dispatcher.release.wait();
    worker.join().expect("worker").expect("dispatch");

    let replay = gateway.execute(&intent, FaultPoint::None).expect("replay");
    assert!(replay.replayed);
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn fault_before_dispatch_is_known_not_sent_and_remains_prepared() {
    let fixture = Fixture::new();
    let dispatcher = Arc::new(IdempotentDispatcher::default());
    let gateway = fixture.gateway(Arc::new(AllowAuthorizer::default()), dispatcher.clone());
    let intent = fixture.intent(json!({"body": "safe"}));

    assert!(matches!(
        gateway.execute(&intent, FaultPoint::AfterPrepared),
        Err(GatewayError::FaultInjected {
            safety: FaultSafety::KnownNotSent,
            ..
        })
    ));
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        gateway
            .inspect(intent.operation_id())
            .expect("inspect")
            .state,
        EffectState::Prepared
    );
}

#[test]
fn faults_at_or_after_dispatch_are_conservatively_unknown_and_never_retried() {
    for point in [
        FaultPoint::AfterDispatchedBeforeInvoke,
        FaultPoint::AfterInvokeBeforeCommit,
    ] {
        let fixture = Fixture::new();
        let dispatcher = Arc::new(IdempotentDispatcher::default());
        let gateway = fixture.gateway(Arc::new(AllowAuthorizer::default()), dispatcher.clone());
        let intent = fixture.intent(json!({"point": format!("{point:?}")}));

        assert!(matches!(
            gateway.execute(&intent, point),
            Err(GatewayError::FaultInjected {
                safety: FaultSafety::UnknownOutcome,
                ..
            })
        ));
        let expected_calls = usize::from(point == FaultPoint::AfterInvokeBeforeCommit);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), expected_calls);
        assert!(matches!(
            gateway.execute(&intent, FaultPoint::None),
            Err(GatewayError::UnresolvedOperation { .. })
        ));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), expected_calls);
    }
}

#[test]
fn provider_credential_fields_are_rejected_before_authorization_or_storage() {
    let fixture = Fixture::new();
    let authorizer = Arc::new(AllowAuthorizer::default());
    let dispatcher = Arc::new(IdempotentDispatcher::default());
    let _gateway = fixture.gateway(authorizer.clone(), dispatcher.clone());

    let error = CanonicalIntent::new(
        fixture.session,
        fixture.branch,
        "turn-9",
        "model.call",
        "provider:demo",
        json!({"headers": {"Authorization": "Bearer must-not-persist"}}),
        3,
    )
    .expect_err("provider credentials must be refused");
    assert!(matches!(error, GatewayError::SensitiveField { .. }));
    assert_eq!(authorizer.calls.load(Ordering::SeqCst), 0);
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);

    let store = Store::open(&fixture.database).expect("store");
    let effects: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM effect_intents", [], |row| row.get(0))
        .expect("count effects");
    assert_eq!(effects, 0);
    assert!(!path_contains(&fixture.blobs, b"must-not-persist"));
}

#[test]
fn transition_history_is_database_enforced_append_only() {
    let fixture = Fixture::new();
    let gateway = fixture.gateway(
        Arc::new(AllowAuthorizer::default()),
        Arc::new(IdempotentDispatcher::default()),
    );
    let intent = fixture.intent(json!({"body": "immutable"}));
    gateway
        .execute(&intent, FaultPoint::None)
        .expect("dispatch");

    let store = Store::open(&fixture.database).expect("store");
    assert!(
        store
            .connection()
            .execute("DELETE FROM effect_transition_history", [])
            .is_err()
    );
    assert!(
        store
            .connection()
            .execute(
                "UPDATE effect_transition_history SET detail_json = '{}'",
                [],
            )
            .is_err()
    );
}

struct InspectingDispatcher {
    database: PathBuf,
    observed_durable_boundary: Mutex<bool>,
}

impl EffectDispatcher for InspectingDispatcher {
    fn dispatch(&self, request: &DispatchRequest<'_>) -> DispatchOutcome {
        let store = Store::open(&self.database).expect("dispatcher can reopen trusted DB");
        let observation: (String, i64, i64) = store
            .connection()
            .query_row(
                "SELECT state, \
                        (SELECT COUNT(*) FROM effect_transition_history h \
                         WHERE h.effect_id = i.id AND h.state = 'requested'), \
                        (SELECT COUNT(*) FROM effect_attempts a \
                         WHERE a.effect_id = i.id AND a.state = 'started') \
                 FROM effect_intents i WHERE operation_id = ?1",
                [request.operation_id().as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("intent is durable before provider invocation");
        *self.observed_durable_boundary.lock().expect("observation") =
            observation == ("dispatched".to_owned(), 1, 1);
        DispatchOutcome::Committed(DispatchResult {
            bytes: b"observed".to_vec(),
            media_type: "text/plain".to_owned(),
            downstream_reference: None,
        })
    }
}

#[test]
fn intent_and_started_attempt_are_durable_before_dispatcher_invocation() {
    let fixture = Fixture::new();
    let dispatcher = Arc::new(InspectingDispatcher {
        database: fixture.database.clone(),
        observed_durable_boundary: Mutex::new(false),
    });
    let gateway = fixture.gateway(Arc::new(AllowAuthorizer::default()), dispatcher.clone());
    gateway
        .execute(&fixture.intent(json!({"body": "inspect"})), FaultPoint::None)
        .expect("dispatch");
    assert!(
        *dispatcher
            .observed_durable_boundary
            .lock()
            .expect("observation")
    );
}

struct FailingDispatcher;

impl EffectDispatcher for FailingDispatcher {
    fn dispatch(&self, _request: &DispatchRequest<'_>) -> DispatchOutcome {
        DispatchOutcome::Failed(DispatchFailureCode::Unavailable)
    }
}

#[test]
fn bounded_dispatch_failure_is_durable_in_transition_and_attempt_history() {
    let fixture = Fixture::new();
    let gateway = fixture.gateway(Arc::new(AllowAuthorizer::default()), Arc::new(FailingDispatcher));
    let intent = fixture.intent(json!({"body": "fail"}));
    assert!(matches!(
        gateway.execute(&intent, FaultPoint::None),
        Err(GatewayError::DispatchFailed {
            code: DispatchFailureCode::Unavailable,
            ..
        })
    ));
    assert_eq!(
        gateway.inspect(intent.operation_id()).expect("inspect").state,
        EffectState::Failed
    );
    assert_eq!(
        gateway
            .attempt_history(intent.operation_id())
            .expect("attempt history")
            .into_iter()
            .map(|entry| entry.state)
            .collect::<Vec<_>>(),
        [AttemptState::Started, AttemptState::Failed]
    );
}

#[test]
fn supplied_deterministic_dispatcher_is_idempotent_and_gateway_replays_it() {
    let fixture = Fixture::new();
    let dispatcher = Arc::new(DeterministicLocalDispatcher::default());
    let gateway = fixture.gateway(Arc::new(AllowAuthorizer::default()), dispatcher.clone());
    let intent = fixture.intent(json!({"body": "fixture"}));
    let first = gateway
        .execute(&intent, FaultPoint::None)
        .expect("first execution");
    let second = gateway
        .execute(&intent, FaultPoint::None)
        .expect("replay");
    assert_eq!(first.result, second.result);
    assert_eq!(dispatcher.dispatch_count(), 1);
}

#[test]
fn duplicate_suppression_holds_across_independent_gateway_connections() {
    const CALLERS: usize = 16;
    let fixture = Fixture::new();
    let dispatcher = Arc::new(IdempotentDispatcher::default());
    let barrier = Arc::new(Barrier::new(CALLERS));
    let handles = (0..CALLERS)
        .map(|_| {
            let database = fixture.database.clone();
            let blobs = fixture.blobs.clone();
            let dispatcher = dispatcher.clone();
            let barrier = barrier.clone();
            let intent = fixture.intent(json!({"body": "cross-connection"}));
            thread::spawn(move || {
                let gateway = EffectGateway::open(
                    database,
                    blobs,
                    Arc::new(AllowAuthorizer::default()),
                    dispatcher,
                )
                .expect("gateway connection");
                barrier.wait();
                gateway.execute(&intent, FaultPoint::None)
            })
        })
        .collect::<Vec<_>>();

    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("caller"))
        .collect::<Vec<_>>();
    assert!(outcomes.iter().any(Result::is_ok));
    assert!(outcomes.iter().all(|outcome| {
        outcome.is_ok()
            || matches!(outcome, Err(GatewayError::UnresolvedOperation { .. }))
    }));
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
}

fn path_contains(root: &Path, needle: &[u8]) -> bool {
    if !root.exists() {
        return false;
    }
    std::fs::read_dir(root)
        .expect("read blob root")
        .filter_map(Result::ok)
        .any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                path_contains(&path, needle)
            } else {
                std::fs::read(path)
                    .is_ok_and(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
            }
        })
}
