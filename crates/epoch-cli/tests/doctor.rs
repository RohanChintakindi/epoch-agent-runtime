use std::process::Command;

#[test]
fn doctor_distinguishes_registered_backends_from_detected_host_tools() {
    let output = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .args(["doctor", "--json"])
        .output()
        .expect("invoke epoch doctor");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor report is JSON");
    assert_eq!(
        report["backends"]["direct_execution"]["status"],
        "supported"
    );
    assert_eq!(
        report["backends"]["direct_execution"]["registered"],
        true
    );
    assert_eq!(
        report["backends"]["application_checkpoint"]["status"],
        "supported"
    );
    assert_eq!(
        report["backends"]["application_checkpoint"]["scope"],
        "application_context_only"
    );

    for name in [
        "process_checkpoint",
        "criu_checkpoint",
        "workspace_checkpoint",
    ] {
        assert_eq!(report["backends"][name]["status"], "unsupported");
        assert_eq!(report["backends"][name]["registered"], false);
        assert!(report["backends"][name]["backend"].is_null());
        assert!(report["backends"][name]["reason"].is_string());
    }

    assert_eq!(
        report["backends"]["criu_checkpoint"]["dependency_detected"],
        report["criu"].is_string()
    );
}
