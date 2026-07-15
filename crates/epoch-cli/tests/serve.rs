use std::{fs, process::Command};

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
