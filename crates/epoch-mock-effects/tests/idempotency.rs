use epoch_mock_effects::{
    DeliveryOutcome, EffectKind, MockEffectError, MockEffectStore, OperationRequest,
};
use serde_json::json;
use tempfile::TempDir;

fn email_request(operation_id: &str, recipient: &str) -> OperationRequest {
    OperationRequest {
        operation_id: operation_id.to_owned(),
        kind: EffectKind::Email,
        payload: json!({
            "recipient": recipient,
            "subject": "Epoch test",
            "body": "deterministic body"
        }),
    }
}

#[test]
fn repeating_an_operation_id_returns_one_committed_result() {
    let directory = TempDir::new().expect("temporary directory");
    let mut store = MockEffectStore::open(directory.path().join("effects.db")).expect("open store");
    let request = email_request("email-001", "alice@example.test");

    let first = store.submit(&request, false).expect("first submission");
    let second = store.submit(&request, false).expect("idempotent retry");

    assert_eq!(first, second);
    let DeliveryOutcome::Respond(committed) = first else {
        panic!("ordinary delivery must respond");
    };
    assert_eq!(
        store.lookup("email-001").expect("lookup committed effect"),
        Some(committed)
    );
}

#[test]
fn reusing_an_operation_id_with_different_content_is_rejected() {
    let directory = TempDir::new().expect("temporary directory");
    let mut store = MockEffectStore::open(directory.path().join("effects.db")).expect("open store");
    store
        .submit(&email_request("email-002", "alice@example.test"), false)
        .expect("first submission");

    let error = store
        .submit(&email_request("email-002", "mallory@example.test"), false)
        .expect_err("changed retry must fail");
    assert!(matches!(
        error,
        MockEffectError::OperationConflict { operation_id } if operation_id == "email-002"
    ));
}

#[test]
fn lost_response_mode_commits_before_withholding_the_response() {
    let directory = TempDir::new().expect("temporary directory");
    let mut store = MockEffectStore::open(directory.path().join("effects.db")).expect("open store");
    let request = email_request("email-003", "alice@example.test");

    assert_eq!(
        store.submit(&request, true).expect("submit effect"),
        DeliveryOutcome::WithholdResponse {
            operation_id: "email-003".to_owned()
        }
    );
    assert!(
        store
            .lookup("email-003")
            .expect("lookup after lost response")
            .is_some(),
        "the remote effect must be committed before its response is lost"
    );
}

#[test]
fn committed_operations_survive_service_restart() {
    let directory = TempDir::new().expect("temporary directory");
    let database = directory.path().join("effects.db");
    let committed = {
        let mut store = MockEffectStore::open(&database).expect("open store");
        let DeliveryOutcome::Respond(committed) = store
            .submit(&email_request("email-004", "alice@example.test"), false)
            .expect("submit effect")
        else {
            panic!("ordinary delivery must respond");
        };
        committed
    };

    let reopened = MockEffectStore::open(&database).expect("reopen store");
    assert_eq!(
        reopened.lookup("email-004").expect("lookup after restart"),
        Some(committed)
    );
}

#[test]
fn concurrent_retries_commit_one_identical_remote_result() {
    use std::sync::{Arc, Barrier};

    let directory = TempDir::new().expect("temporary directory");
    let database = Arc::new(directory.path().join("effects.db"));
    MockEffectStore::open(database.as_path()).expect("initialize store");
    let request = Arc::new(email_request("email-005", "alice@example.test"));
    let barrier = Arc::new(Barrier::new(8));
    let workers = (0..8)
        .map(|_| {
            let database = Arc::clone(&database);
            let request = Arc::clone(&request);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let mut store = MockEffectStore::open(database.as_path()).expect("open store");
                barrier.wait();
                let DeliveryOutcome::Respond(committed) = store
                    .submit(&request, false)
                    .expect("concurrent idempotent submission")
                else {
                    panic!("ordinary delivery must respond");
                };
                committed
            })
        })
        .collect::<Vec<_>>();

    let committed = workers
        .into_iter()
        .map(|worker| worker.join().expect("worker must not panic"))
        .collect::<Vec<_>>();
    assert!(
        committed.windows(2).all(|pair| pair[0] == pair[1]),
        "every retry must observe the same committed result"
    );
}
