use std::{
    fs,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

use epoch_storage::Store;
use tempfile::TempDir;

fn epoch(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args(arguments)
        .output()
        .expect("launch epoch")
}

#[test]
fn serve_refuses_non_loopback_before_opening_state() {
    let root = TempDir::new().expect("fixture root");
    let missing = root.path().join("missing");
    let output = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args(["serve", "--state-root"])
        .arg(&missing)
        .args(["--bind", "0.0.0.0:8080"])
        .output()
        .expect("launch epoch");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("refuses non-loopback"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!missing.exists());
}

#[test]
fn serve_reports_missing_and_corrupt_state_without_creating_it() {
    let root = TempDir::new().expect("fixture root");
    let missing = root.path().join("missing");
    let missing_output = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args(["serve", "--state-root"])
        .arg(&missing)
        .args(["--bind", "127.0.0.1:8080"])
        .output()
        .expect("launch epoch");
    assert_eq!(missing_output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing_output.stderr).contains("does not exist"));
    assert!(!missing.exists());

    let corrupt = root.path().join("corrupt");
    fs::create_dir(&corrupt).expect("corrupt root");
    fs::write(corrupt.join("state.db"), b"not SQLite").expect("corrupt database");
    let corrupt_output = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args(["serve", "--state-root"])
        .arg(&corrupt)
        .args(["--bind", "127.0.0.1:8080"])
        .output()
        .expect("launch epoch");
    assert_eq!(corrupt_output.status.code(), Some(125));
    assert!(
        String::from_utf8_lossy(&corrupt_output.stderr)
            .contains("trusted dashboard state is unavailable")
    );
}

#[test]
fn serve_help_documents_local_state_and_optional_results_roots() {
    let output = epoch(&["serve", "--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("--state-root"));
    assert!(help.contains("--results-root"));
    assert!(help.contains("127.0.0.1:8080"));
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn serve_exposes_read_only_json_with_security_headers_on_loopback() {
    let root = TempDir::new().expect("state root");
    Store::open(root.path().join("state.db")).expect("empty trusted state");
    let reservation = TcpListener::bind("127.0.0.1:0").expect("reserve port");
    let address = reservation.local_addr().expect("reserved address");
    drop(reservation);

    let child = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args(["serve", "--state-root"])
        .arg(root.path())
        .args(["--bind", &address.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start dashboard server");
    let mut child = ChildGuard(child);

    let mut stream = (0..100)
        .find_map(|_| match TcpStream::connect(address) {
            Ok(stream) => Some(stream),
            Err(_) => {
                assert!(
                    child.0.try_wait().expect("server state").is_none(),
                    "dashboard exited before accepting connections"
                );
                thread::sleep(Duration::from_millis(10));
                None
            }
        })
        .expect("dashboard listener");
    stream
        .write_all(b"GET /api/v1/sessions?limit=1&offset=0 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("send request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("Content-Security-Policy: default-src 'none'"));
    assert!(response.contains("X-Content-Type-Options: nosniff"));
    assert!(response.contains("Cache-Control: no-store"));
    assert!(response.contains(
        r#"{"items":[],"page":{"offset":0,"limit":1,"has_more":false,"next_offset":null}}"#
    ));
}
