use std::sync::Arc;

use epoch_capabilities::{
    CapabilityAuthorizer, CapabilityConstraints, CapabilityService, IssueRequest,
};
use epoch_core::{BranchId, SessionId};
use epoch_effects::{
    CanonicalIntent, DeterministicLocalDispatcher, EffectGateway, FaultPoint, GatewayError,
};
use epoch_storage::Store;
use rusqlite::params;
use serde_json::json;
use tempfile::TempDir;

#[test]
fn effect_gateway_adapter_consumes_current_branch_bound_capability() {
    let directory = TempDir::new().expect("runtime");
    let database = directory.path().join("state.db");
    let blobs = directory.path().join("blobs");
    let session = SessionId::new();
    let branch = BranchId::new();
    let store = Store::open(&database).expect("store");
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
            "INSERT INTO branches (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
         VALUES (?1, ?2, 'running', 0, 0)",
            params![branch.to_string(), session.to_string()],
        )
        .expect("branch");
    drop(store);

    let service = Arc::new(CapabilityService::open(&database).expect("service"));
    service
        .set_policy_revision(session, branch, 3)
        .expect("policy");
    let issued = service
        .issue(&IssueRequest {
            session_id: session,
            branch_id: branch,
            subject: "agent-1".to_owned(),
            action: "email.send".to_owned(),
            resource: "mailbox:test".to_owned(),
            constraints: CapabilityConstraints {
                max_uses: Some(1),
                budget_units: Some(1),
            },
            expires_at_unix_ms: None,
            policy_revision: 3,
        })
        .expect("capability");
    let capability_id = issued.capability_id;
    let authorizer = Arc::new(
        CapabilityAuthorizer::new(service.clone(), issued.handle, "agent-1", 1).expect("adapter"),
    );
    let gateway = EffectGateway::open(
        &database,
        &blobs,
        authorizer,
        Arc::new(DeterministicLocalDispatcher::default()),
    )
    .expect("gateway");
    let intent = CanonicalIntent::new(
        session,
        branch,
        "turn-1/email-1",
        "email.send",
        "mailbox:test",
        json!({"to": "a@example.test"}),
        3,
    )
    .expect("intent");

    gateway
        .execute(&intent, FaultPoint::None)
        .expect("authorized effect");
    let recorded_capability: Option<String> = Store::open(&database)
        .expect("reopen state")
        .connection()
        .query_row(
            "SELECT capability_id FROM effect_intents WHERE operation_id = ?1",
            [intent.operation_id().as_str()],
            |row| row.get(0),
        )
        .expect("read effect authority binding");
    assert_eq!(recorded_capability, Some(capability_id.to_string()));

    let second = CanonicalIntent::new(
        session,
        branch,
        "turn-2/email-1",
        "email.send",
        "mailbox:test",
        json!({"to": "b@example.test"}),
        3,
    )
    .expect("intent");
    assert!(matches!(
        gateway.execute(&second, FaultPoint::None),
        Err(GatewayError::AuthorizationDenied { .. })
    ));
    assert_eq!(service.audit_history().expect("audit").len(), 2);
}

#[test]
fn failed_effect_intent_insert_rolls_back_one_use_capability_consumption() {
    let directory = TempDir::new().expect("runtime");
    let database = directory.path().join("state.db");
    let blobs = directory.path().join("blobs");
    let session = SessionId::new();
    let branch = BranchId::new();
    let store = Store::open(&database).expect("store");
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
            "INSERT INTO branches (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
             VALUES (?1, ?2, 'running', 0, 0)",
            params![branch.to_string(), session.to_string()],
        )
        .expect("branch");
    drop(store);

    let service = Arc::new(CapabilityService::open(&database).expect("service"));
    service
        .set_policy_revision(session, branch, 1)
        .expect("policy");
    let issued = service
        .issue(&IssueRequest {
            session_id: session,
            branch_id: branch,
            subject: "agent-1".to_owned(),
            action: "email.send".to_owned(),
            resource: "mailbox:test".to_owned(),
            constraints: CapabilityConstraints {
                max_uses: Some(1),
                budget_units: Some(1),
            },
            expires_at_unix_ms: None,
            policy_revision: 1,
        })
        .expect("capability");
    let authorizer = Arc::new(
        CapabilityAuthorizer::new(service.clone(), issued.handle, "agent-1", 1).expect("adapter"),
    );
    let gateway = EffectGateway::open(
        &database,
        &blobs,
        authorizer,
        Arc::new(DeterministicLocalDispatcher::default()),
    )
    .expect("gateway");
    let intent = CanonicalIntent::new(
        session,
        branch,
        "turn-atomic/email-1",
        "email.send",
        "mailbox:test",
        json!({"to": "atomic@example.test"}),
        1,
    )
    .expect("intent");

    let store = Store::open(&database).expect("trigger state");
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER test_reject_effect_intent \
             BEFORE INSERT ON effect_intents BEGIN \
                 SELECT RAISE(ABORT, 'injected effect intent failure'); \
             END;",
        )
        .expect("install failure trigger");
    assert!(matches!(
        gateway.execute(&intent, FaultPoint::None),
        Err(GatewayError::Database(_))
    ));
    store
        .connection()
        .execute_batch("DROP TRIGGER test_reject_effect_intent;")
        .expect("remove failure trigger");
    drop(store);

    gateway
        .execute(&intent, FaultPoint::None)
        .expect("authority consumption must roll back with failed intent");
    assert_eq!(service.audit_history().expect("audit").len(), 1);
}
