use std::{
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    thread,
    time::Duration,
};

use epoch_mock_effects::{MockEffectServer, MockEffectStore};
use serde_json::{Value, json};
use tempfile::TempDir;

fn request(address: SocketAddr, method: &str, path: &str, body: Option<&Value>) -> Vec<u8> {
    let body = body.map_or_else(Vec::new, |value| {
        serde_json::to_vec(value).expect("serialize request")
    });
    let mut stream = TcpStream::connect(address).expect("connect to mock service");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("write request headers");
    stream.write_all(&body).expect("write request body");
    stream.shutdown(Shutdown::Write).expect("finish request");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

fn response_body(response: &[u8]) -> Value {
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response separator");
    serde_json::from_slice(&response[separator + 4..]).expect("JSON response body")
}

fn operation(operation_id: &str, recipient: &str, withhold_response: bool) -> Value {
    json!({
        "operation_id": operation_id,
        "kind": "email",
        "payload": {"recipient": recipient, "body": "hello"},
        "withhold_response": withhold_response
    })
}

#[test]
fn local_http_service_commits_and_exposes_status_lookup() {
    let directory = TempDir::new().expect("temporary directory");
    let database = directory.path().join("effects.db");
    let mut server = MockEffectServer::bind("127.0.0.1:0", &database).expect("bind service");
    let address = server.local_addr();
    let worker = thread::spawn(move || {
        server.handle_one().expect("handle submission");
        server.handle_one().expect("handle status lookup");
    });

    let submitted = request(
        address,
        "POST",
        "/v1/operations",
        Some(&operation("http-001", "alice@example.test", false)),
    );
    assert!(submitted.starts_with(b"HTTP/1.1 200"));
    assert_eq!(response_body(&submitted)["operation_id"], "http-001");

    let status = request(address, "GET", "/v1/operations/http-001", None);
    assert!(status.starts_with(b"HTTP/1.1 200"));
    assert_eq!(response_body(&status)["operation_id"], "http-001");
    worker.join().expect("service thread");
}

#[test]
fn local_http_service_returns_conflict_for_changed_idempotency_payload() {
    let directory = TempDir::new().expect("temporary directory");
    let mut server = MockEffectServer::bind("127.0.0.1:0", directory.path().join("effects.db"))
        .expect("bind service");
    let address = server.local_addr();
    let worker = thread::spawn(move || {
        server.handle_one().expect("handle first submission");
        server.handle_one().expect("handle conflicting submission");
    });

    let first = request(
        address,
        "POST",
        "/v1/operations",
        Some(&operation("http-002", "alice@example.test", false)),
    );
    assert!(first.starts_with(b"HTTP/1.1 200"));
    let conflict = request(
        address,
        "POST",
        "/v1/operations",
        Some(&operation("http-002", "mallory@example.test", false)),
    );
    assert!(conflict.starts_with(b"HTTP/1.1 409"));
    worker.join().expect("service thread");
}

#[test]
fn lost_response_mode_closes_without_reply_but_status_remains_committed() {
    let directory = TempDir::new().expect("temporary directory");
    let database = directory.path().join("effects.db");
    let mut server = MockEffectServer::bind("127.0.0.1:0", &database).expect("bind service");
    let address = server.local_addr();
    let worker = thread::spawn(move || server.handle_one().expect("handle lost response"));

    let response = request(
        address,
        "POST",
        "/v1/operations",
        Some(&operation("http-003", "alice@example.test", true)),
    );
    assert!(response.is_empty(), "lost response must emit no HTTP reply");
    worker.join().expect("service thread");

    let reopened = MockEffectStore::open(database).expect("reopen committed store");
    assert!(
        reopened
            .lookup("http-003")
            .expect("lookup after lost response")
            .is_some()
    );
}

#[test]
fn mock_service_refuses_non_loopback_listeners() {
    let directory = TempDir::new().expect("temporary directory");
    assert!(
        MockEffectServer::bind("0.0.0.0:0", directory.path().join("effects.db")).is_err(),
        "the test effect service must not become a network-exposed side effect endpoint"
    );
}
