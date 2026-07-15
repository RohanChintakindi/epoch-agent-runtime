use std::{process::Command, str::FromStr as _, sync::Arc};

use epoch_capabilities::{CapabilityAuthorizer, CapabilityHandle, CapabilityService};
use epoch_core::{BranchId, SessionId};
use epoch_effects::{CanonicalIntent, DeterministicLocalDispatcher, EffectGateway, FaultPoint};
use epoch_storage::Store;
use rusqlite::params;
use serde_json::json;
use tempfile::TempDir;

fn epoch(fixture: &TempDir, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(arguments)
        .output()
        .expect("launch epoch CLI")
}

fn runtime() -> (TempDir, SessionId, BranchId) {
    let fixture = TempDir::new().expect("runtime");
    let state_root = fixture.path().join(".epoch");
    std::fs::create_dir(&state_root).expect("state root");
    let store = Store::open(state_root.join("state.db")).expect("state");
    let session = SessionId::new();
    let branch = BranchId::new();
    store
        .connection()
        .execute(
            "INSERT INTO sessions \
             (id, state, policy_revision, created_at_unix_ms, updated_at_unix_ms) \
             VALUES (?1, 'running', 9, 0, 0)",
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
    (fixture, session, branch)
}

#[test]
fn capability_and_effect_commands_have_stable_non_secret_json_contracts() {
    let (fixture, session, branch) = runtime();
    let constraints = json!({
        "subject": "agent-1",
        "resource": "mailbox:test",
        "max_uses": 2,
        "budget_units": 2
    })
    .to_string();
    let grant = epoch(
        &fixture,
        &[
            "capability",
            "grant",
            &branch.to_string(),
            "email.send",
            &constraints,
        ],
    );
    assert!(
        grant.status.success(),
        "grant failed: {}",
        String::from_utf8_lossy(&grant.stderr)
    );
    let grant: serde_json::Value = serde_json::from_slice(&grant.stdout).expect("grant JSON");
    let capability_id = grant["capability_id"].as_str().expect("capability ID");
    let handle = grant["handle"].as_str().expect("one-time bearer handle");
    assert_eq!(grant["session_id"], session.to_string());
    assert_eq!(grant["branch_id"], branch.to_string());
    assert_eq!(grant["policy_revision"], 9);

    let inspect = epoch(&fixture, &["capability", "inspect", capability_id]);
    assert!(inspect.status.success());
    let inspect_output = String::from_utf8_lossy(&inspect.stdout);
    let inspect: serde_json::Value = serde_json::from_slice(&inspect.stdout).expect("inspect JSON");
    assert_eq!(inspect["capability_id"], capability_id);
    assert_eq!(inspect["state"], "active");
    assert!(
        !inspect_output.contains(handle),
        "inspection must not re-expose bearer authority"
    );

    let service = Arc::new(
        CapabilityService::open(fixture.path().join(".epoch/state.db")).expect("authority"),
    );
    let authorizer = Arc::new(
        CapabilityAuthorizer::new(
            service,
            CapabilityHandle::from_str(handle).expect("handle"),
            "agent-1",
            1,
        )
        .expect("adapter"),
    );
    let gateway = EffectGateway::open(
        fixture.path().join(".epoch/state.db"),
        fixture.path().join(".epoch/blobs"),
        authorizer,
        Arc::new(DeterministicLocalDispatcher::default()),
    )
    .expect("gateway");
    let intent = CanonicalIntent::new(
        session,
        branch,
        "cli-security/effect-1",
        "email.send",
        "mailbox:test",
        json!({"to": "demo@example.test"}),
        9,
    )
    .expect("intent");
    gateway
        .execute(&intent, FaultPoint::None)
        .expect("authorized effect");

    let effects = epoch(&fixture, &["effects", "list", &session.to_string()]);
    assert!(effects.status.success());
    let effects: serde_json::Value =
        serde_json::from_slice(&effects.stdout).expect("effect history JSON");
    assert_eq!(effects.as_array().expect("effect array").len(), 1);
    assert_eq!(effects[0]["capability_id"], capability_id);
    assert_eq!(effects[0]["operation_id"], intent.operation_id().as_str());
    assert_eq!(effects[0]["state"], "committed");

    let revoke = epoch(&fixture, &["capability", "revoke", capability_id]);
    assert!(revoke.status.success());
    let revoked: serde_json::Value = serde_json::from_slice(&revoke.stdout).expect("revoke JSON");
    assert_eq!(revoked["state"], "revoked");
}

#[test]
fn capability_grant_rejects_ambiguous_or_unknown_constraints() {
    let (fixture, _session, branch) = runtime();
    for constraints in [
        "not-json",
        r#"{"subject":"agent","resource":"mailbox:test","surprise":true}"#,
        r#"{"subject":"agent"}"#,
    ] {
        let output = epoch(
            &fixture,
            &[
                "capability",
                "grant",
                &branch.to_string(),
                "email.send",
                constraints,
            ],
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("constraints"));
    }
}
