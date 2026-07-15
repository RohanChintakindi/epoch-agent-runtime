use std::fs;

use epoch_dashboard::{Dashboard, parse_loopback_bind};
use epoch_storage::Store;
use rusqlite::params;
use serde_json::Value;
use tempfile::TempDir;
use uuid::Uuid;

const SESSION: &str = "10000000-0000-4000-8000-000000000001";
const ROOT_BRANCH: &str = "20000000-0000-4000-8000-000000000001";
const CHILD_BRANCH: &str = "20000000-0000-4000-8000-000000000002";
const EPOCH: &str = "30000000-0000-4000-8000-000000000001";
const EPOCH_TWO: &str = "30000000-0000-4000-8000-000000000002";
const SECRET: &str = "ecap_v1_deaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddeaddead";

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn populated() -> Self {
        let root = TempDir::new().expect("temporary state root");
        let store = Store::open(root.path().join("state.db")).expect("migrate fixture");
        let connection = store.connection();
        connection
            .execute(
                "INSERT INTO sessions (id, state, policy_revision, revision, created_at_unix_ms, updated_at_unix_ms) VALUES (?1, 'completed', 3, 7, 10, 90)",
                [SESSION],
            )
            .expect("session");
        connection
            .execute(
                "INSERT INTO branches (id, session_id, state, next_event_sequence, created_at_unix_ms, updated_at_unix_ms) VALUES (?1, ?2, 'completed', 2, 10, 90)",
                params![ROOT_BRANCH, SESSION],
            )
            .expect("root branch");
        connection
            .execute(
                "INSERT INTO epochs (id, session_id, branch_id, sequence, status, backend, policy_revision, effect_frontier, capability_frontier, created_at_unix_ms, committed_at_unix_ms) VALUES (?1, ?2, ?3, 0, 'committed', 'cooperative-w02-v1', 3, 1, 1, 20, 30)",
                params![EPOCH, SESSION, ROOT_BRANCH],
            )
            .expect("epoch");
        connection
            .execute(
                "INSERT INTO blobs (hash, byte_length, media_type, created_at_unix_ms) VALUES (?1, 1, 'application/octet-stream', 20)",
                ["a".repeat(64)],
            )
            .expect("blob");
        connection
            .execute(
                "INSERT INTO snapshot_components (epoch_id, kind, status, backend, blob_hash, checksum_sha256, byte_length, metadata_json, staged_at_unix_ms, committed_at_unix_ms) VALUES (?1, 'application_context', 'committed', 'cooperative-w02-v1', ?2, ?2, 1, '{\"boundary_sequence\":1}', 20, 30)",
                params![EPOCH, "a".repeat(64)],
            )
            .expect("component");
        connection
            .execute(
                "INSERT INTO branches (id, session_id, parent_branch_id, fork_epoch_id, state, next_event_sequence, created_at_unix_ms, updated_at_unix_ms, name, fork_point_sequence, fork_component_hash) VALUES (?1, ?2, ?3, ?4, 'running', 1, 40, 80, 'candidate', 1, ?5)",
                params![CHILD_BRANCH, SESSION, ROOT_BRANCH, EPOCH, "a".repeat(64)],
            )
            .expect("child branch");
        for (id, branch, sequence, actor, kind, status, payload) in [
            (
                "40000000-0000-4000-8000-000000000001",
                ROOT_BRANCH,
                0_i64,
                "supervisor",
                "supervisor.run_started",
                "started",
                "{}",
            ),
            (
                "40000000-0000-4000-8000-000000000002",
                ROOT_BRANCH,
                1,
                "agent",
                "application.context_restored",
                "succeeded",
                "{}",
            ),
            (
                "40000000-0000-4000-8000-000000000003",
                CHILD_BRANCH,
                0,
                "tool",
                "tool.output",
                "failed",
                &format!("{{\"stderr\":\"{SECRET}\"}}"),
            ),
        ] {
            connection
                .execute(
                    "INSERT INTO events (id, session_id, branch_id, sequence, epoch_id, monotonic_ns, occurred_at_unix_ms, actor, kind, status, payload_json) VALUES (?1, ?2, ?3, ?4, CASE WHEN ?5 = 'application.context_restored' THEN ?6 ELSE NULL END, ?4, 50 + ?4, ?7, ?5, ?8, ?9)",
                    params![id, SESSION, branch, sequence, kind, EPOCH, actor, status, payload],
                )
                .expect("event");
        }
        let effect_id = "50000000-0000-4000-8000-000000000001";
        connection.execute(
            "INSERT INTO effect_intents (id, session_id, branch_id, operation_id, replay_key, action, resource, input_hash, state, error_json, policy_revision, prepared_at_unix_ms, dispatched_at_unix_ms, resolved_at_unix_ms, revision) VALUES (?1, ?2, ?3, 'op_fixture', 'step-1', 'email.send', 'mailbox:test', ?4, 'failed', ?5, 3, 60, 61, 62, 2)",
            params![effect_id, SESSION, ROOT_BRANCH, "a".repeat(64), format!(r#"{{"provider_error":"{SECRET}"}}"#)],
        ).expect("effect intent");
        connection.execute(
            "INSERT INTO effect_attempts (id, effect_id, attempt_no, state, downstream_idempotency_key, error_json, started_at_unix_ms, completed_at_unix_ms) VALUES ('60000000-0000-4000-8000-000000000001', ?1, 1, 'failed', 'idem-1', ?2, 61, 62)",
            params![effect_id, format!(r#"{{"raw":"{SECRET}"}}"#)],
        ).expect("effect attempt");
        connection.execute(
            "INSERT INTO effect_transition_history (effect_id, sequence, state, occurred_at_unix_ms, detail_json) VALUES (?1, 0, 'requested', 60, ?2), (?1, 1, 'failed', 62, ?2)",
            params![effect_id, format!(r#"{{"detail":"{SECRET}"}}"#)],
        ).expect("effect transitions");
        connection.execute(
            "INSERT INTO effect_attempt_history (effect_id, attempt_no, sequence, state, occurred_at_unix_ms, detail_json) VALUES (?1, 1, 0, 'started', 61, ?2), (?1, 1, 1, 'failed', 62, ?2)",
            params![effect_id, format!(r#"{{"detail":"{SECRET}"}}"#)],
        ).expect("attempt history");
        connection.execute(
            "INSERT INTO capability_decisions (decision_id, capability_id, handle_hash, session_id, branch_id, subject, action, resource, request_id, request_hash, policy_revision, budget_units, outcome, reason, decided_at_unix_ms) VALUES ('decision-fixture', NULL, ?1, ?2, ?3, 'agent', 'email.send', 'mailbox:test', 'request-1', ?4, 3, 1, 'deny', 'unknown_handle', 63)",
            params!["d".repeat(64), SESSION, ROOT_BRANCH, "e".repeat(64)],
        ).expect("capability audit");
        Self { root }
    }

    fn dashboard(&self) -> Dashboard {
        Dashboard::open(self.root.path(), None).expect("open dashboard")
    }
}

fn json(response: epoch_dashboard::DashboardResponse) -> Value {
    assert_eq!(
        response.status,
        200,
        "{}",
        String::from_utf8_lossy(&response.body)
    );
    serde_json::from_slice(&response.body).expect("JSON response")
}

#[test]
fn refuses_non_loopback_binds() {
    assert!(parse_loopback_bind("127.0.0.1:8080").is_ok());
    assert!(parse_loopback_bind("[::1]:8080").is_ok());
    for bind in ["0.0.0.0:8080", "192.0.2.8:8080", "[::]:8080"] {
        assert!(parse_loopback_bind(bind).is_err(), "accepted {bind}");
    }
}

#[test]
fn rejects_traversal_and_mutation_and_serves_locked_down_assets() {
    let fixture = Fixture::populated();
    let dashboard = fixture.dashboard();
    for target in [
        "/../state.db",
        "/assets/../state.db",
        "/%2e%2e/state.db",
        "/assets%2fapp.js",
    ] {
        assert!(
            dashboard.handle("GET", target).status >= 400,
            "accepted {target}"
        );
    }
    assert_eq!(dashboard.handle("POST", "/api/v1/sessions").status, 405);
    let page = dashboard.handle("GET", "/");
    assert_eq!(page.status, 200);
    assert!(page.headers.iter().any(|(name, value)| {
        *name == "Content-Security-Policy" && value.starts_with("default-src 'none'")
    }));
    assert!(
        page.headers
            .contains(&("X-Content-Type-Options", "nosniff"))
    );
    let html = String::from_utf8(page.body).expect("HTML");
    assert!(!html.contains("https://"));
    assert!(!html.contains("http://"));
    let script = dashboard.handle("GET", "/assets/app.js");
    assert_eq!(script.status, 200);
    assert!(
        !String::from_utf8(script.body)
            .unwrap()
            .contains("innerHTML")
    );
}

#[test]
fn reports_branch_lineage_and_ordered_filtered_timeline() {
    let fixture = Fixture::populated();
    let dashboard = fixture.dashboard();
    let session = json(dashboard.handle("GET", &format!("/api/v1/sessions/{SESSION}")));
    assert_eq!(session["branches"].as_array().unwrap().len(), 2);
    assert_eq!(session["branches"][1]["parent_branch_id"], ROOT_BRANCH);
    assert_eq!(session["branches"][1]["fork_epoch_id"], EPOCH);
    assert_eq!(session["branches"][1]["name"], "candidate");

    let timeline = json(dashboard.handle(
        "GET",
        &format!(
            "/api/v1/branches/{ROOT_BRANCH}/timeline?actor=agent&status=succeeded&limit=1&offset=0"
        ),
    ));
    assert_eq!(timeline["items"].as_array().unwrap().len(), 1);
    assert_eq!(timeline["items"][0]["sequence"], 1);
    assert_eq!(timeline["items"][0]["kind"], "application.context_restored");
    assert_eq!(timeline["page"]["has_more"], false);

    let epochs = json(dashboard.handle(
        "GET",
        &format!("/api/v1/sessions/{SESSION}/epochs?limit=10&offset=0"),
    ));
    assert_eq!(
        epochs["items"][0]["components"][0]["kind"],
        "application_context"
    );
    assert_eq!(
        epochs["items"][0]["restore_outcomes"][0]["status"],
        "succeeded"
    );
}

#[test]
fn pagination_is_bounded_and_continuations_do_not_overlap() {
    let fixture = Fixture::populated();
    let dashboard = fixture.dashboard();
    let first = json(dashboard.handle(
        "GET",
        &format!("/api/v1/branches/{ROOT_BRANCH}/timeline?limit=1&offset=0"),
    ));
    let second = json(dashboard.handle(
        "GET",
        &format!("/api/v1/branches/{ROOT_BRANCH}/timeline?limit=1&offset=1"),
    ));
    assert_eq!(first["page"]["has_more"], true);
    assert_eq!(first["page"]["next_offset"], 1);
    assert_ne!(
        first["items"][0]["event_id"],
        second["items"][0]["event_id"]
    );
    assert_eq!(
        dashboard
            .handle(
                "GET",
                &format!("/api/v1/branches/{ROOT_BRANCH}/timeline?limit=201")
            )
            .status,
        400
    );
}

#[test]
fn raw_payloads_and_capability_material_never_reach_responses() {
    let fixture = Fixture::populated();
    let store = Store::open(fixture.root.path().join("state.db")).expect("fixture store");
    store.connection().execute(
        "INSERT INTO capabilities (id, session_id, branch_id, subject, action, resource, constraints_json, handle_hash, remaining_uses, policy_revision, status, issued_at_unix_ms, updated_at_unix_ms, remaining_budget_units) VALUES (?1, ?2, ?3, 'agent', 'email.send', 'mailbox:test', '{}', ?4, 2, 3, 'active', 60, 60, 4)",
        params![Uuid::new_v4().to_string(), SESSION, ROOT_BRANCH, "b".repeat(64)],
    ).expect("capability");
    drop(store);
    let dashboard = fixture.dashboard();
    for target in [
        format!("/api/v1/branches/{CHILD_BRANCH}/timeline"),
        format!("/api/v1/sessions/{SESSION}/capabilities"),
        format!("/api/v1/sessions/{SESSION}/effects"),
    ] {
        let response = dashboard.handle("GET", &target);
        assert_eq!(response.status, 200);
        let body = String::from_utf8(response.body).expect("UTF-8 JSON");
        assert!(!body.contains(SECRET));
        assert!(!body.contains("handle_hash"));
        assert!(!body.contains("payload_json"));
        assert!(!body.contains("error_json"));
    }
    let capabilities =
        json(dashboard.handle("GET", &format!("/api/v1/sessions/{SESSION}/capabilities")));
    assert_eq!(capabilities["current"].as_array().unwrap().len(), 1);
    assert_eq!(capabilities["audit"][0]["reason"], "unknown_handle");
    assert_eq!(capabilities["bearer_material_exposed"], false);
    let effects = json(dashboard.handle("GET", &format!("/api/v1/sessions/{SESSION}/effects")));
    assert_eq!(
        effects["intents"][0]["attempts"][0]["history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        effects["intents"][0]["transitions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(effects["provider_content_exposed"], false);
}

#[test]
fn missing_and_corrupt_databases_fail_without_creation() {
    let missing = TempDir::new().expect("missing fixture");
    assert!(Dashboard::open(missing.path(), None).is_err());
    assert!(!missing.path().join("state.db").exists());

    let corrupt = TempDir::new().expect("corrupt fixture");
    fs::write(corrupt.path().join("state.db"), b"not sqlite").expect("corrupt database");
    assert!(Dashboard::open(corrupt.path(), None).is_err());
}

#[test]
fn json_and_ui_render_injected_text_as_data() {
    let fixture = Fixture::populated();
    let store = Store::open(fixture.root.path().join("state.db")).expect("fixture store");
    store.connection().execute(
        "INSERT INTO blobs (hash, byte_length, media_type, created_at_unix_ms) VALUES (?1, 2, 'application/json', 60)",
        ["c".repeat(64)],
    ).expect("diff blob");
    store.connection().execute(
        "INSERT INTO epochs (id, session_id, branch_id, parent_epoch_id, sequence, status, backend, policy_revision, effect_frontier, capability_frontier, created_at_unix_ms, committed_at_unix_ms) VALUES (?1, ?2, ?3, ?4, 1, 'committed', 'cooperative-w02-v1', 3, 1, 1, 60, 65)",
        params![EPOCH_TWO, SESSION, ROOT_BRANCH, EPOCH],
    ).expect("second epoch");
    let injected = "</script><script>alert(1)</script>";
    store.connection().execute(
        "INSERT INTO semantic_diffs (id, left_epoch_id, right_epoch_id, schema_version, digest, blob_hash, summary_json, created_at_unix_ms) VALUES (?1, ?2, ?3, 1, 'digest', ?4, ?5, 70)",
        params![Uuid::new_v4().to_string(), EPOCH, EPOCH_TWO, "c".repeat(64), format!(r#"{{"identical":false,"changes":[{{"section":"messages","path":"{injected}","classification":"changed","before":"secret","after":"secret"}}]}}"#)],
    ).expect("semantic diff");
    drop(store);

    let dashboard = fixture.dashboard();
    let response = dashboard.handle("GET", &format!("/api/v1/sessions/{SESSION}/diffs"));
    assert_eq!(response.status, 200);
    let body = String::from_utf8(response.body).expect("UTF-8 JSON");
    assert!(!body.contains("<script>"));
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["items"][0]["changes"][0]["path"],
        injected
    );
}

#[test]
fn benchmark_cards_are_real_bounded_reports_or_explicitly_unavailable() {
    let fixture = Fixture::populated();
    let dashboard = fixture.dashboard();
    let unavailable = json(dashboard.handle("GET", "/api/v1/benchmarks"));
    assert_eq!(unavailable["available"], false);
    assert_eq!(unavailable["reason"], "results_directory_missing");

    let results = fixture.root.path().join("results");
    fs::create_dir(&results).expect("results directory");
    fs::write(
        results.join("checkpoint.json"),
        br#"{
          "schema_version": 1,
          "config": {"suite":"checkpoint","backend":"cooperative-w02-v1","trace_mode":"on","repetitions":5},
          "summary": {"succeeded":5,"unsupported":0,"failed":0,"latency_ns":{"p50":1200,"p95":1800,"p99":1900}}
        }"#,
    )
    .expect("benchmark report");
    let dashboard = Dashboard::open(fixture.root.path(), Some(results)).expect("dashboard");
    let available = json(dashboard.handle("GET", "/api/v1/benchmarks"));
    assert_eq!(available["available"], true);
    assert_eq!(available["reports"][0]["suite"], "checkpoint");
    assert_eq!(available["reports"][0]["p95_ns"], 1800);
}
