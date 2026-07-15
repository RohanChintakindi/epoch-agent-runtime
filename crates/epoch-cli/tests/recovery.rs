#![cfg(unix)]

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt as _, symlink},
    path::{Path, PathBuf},
    process::Command,
    str::FromStr as _,
    sync::Arc,
};

use epoch_blob::BlobHash;
use epoch_capabilities::{
    CapabilityAuthorizer, CapabilityHandle, CapabilityService, CapabilityUse, DecisionOutcome,
    DenialReason,
};
use epoch_checkpoint::{APPLICATION_CONTEXT_SCHEMA_VERSION, ApplicationContext, ResumeCursors};
use epoch_core::{BranchId, SessionId};
use epoch_effects::{CanonicalIntent, DeterministicLocalDispatcher, EffectGateway, FaultPoint};
use serde::Serialize;
use serde_json::json;
use tempfile::TempDir;

#[derive(Serialize)]
struct FixtureState {
    seed: u64,
    scenario: &'static str,
    model_response_hash: BlobHash,
    files: BTreeMap<String, BlobHash>,
    memory: Option<serde_json::Value>,
    child: Option<serde_json::Value>,
    network: Option<serde_json::Value>,
    completed_tools: Vec<String>,
}

#[derive(Serialize)]
struct FixtureSummary {
    state: FixtureState,
    state_hash: BlobHash,
    normalized_trace_hash: BlobHash,
    event_count: u64,
    checkpoint_context: ApplicationContext,
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn fixture_manifest(fixture: &TempDir, seed: u64) -> (PathBuf, PathBuf) {
    let safe_point_id = format!("safe-point-files-{seed:016x}");
    let state = FixtureState {
        seed,
        scenario: "files",
        model_response_hash: BlobHash::digest(b"recorded model response"),
        files: BTreeMap::new(),
        memory: None,
        child: None,
        network: None,
        completed_tools: Vec::new(),
    };
    let state_hash = BlobHash::digest(&serde_json::to_vec(&state).expect("encode fixture state"));
    let context = ApplicationContext {
        schema_version: APPLICATION_CONTEXT_SCHEMA_VERSION,
        safe_point_id: safe_point_id.clone(),
        deterministic_seed: seed,
        context_revision: 1,
        cursors: ResumeCursors {
            boundary_sequence: 2,
            message_cursor: 2,
            tool_cursor: 0,
            task_cursor: 0,
        },
        model_identifier: "recorded-model-v1".to_owned(),
        tool_registry: BTreeMap::new(),
        messages: Vec::new(),
        pending_tasks: Vec::new(),
        pending_model_request_ids: Vec::new(),
        pending_tool_call_ids: Vec::new(),
        user_visible_summary_hash: None,
    };
    let summary = serde_json::to_string(&FixtureSummary {
        state,
        state_hash: state_hash.clone(),
        normalized_trace_hash: BlobHash::digest(b"fixture trace"),
        event_count: 4,
        checkpoint_context: context,
    })
    .expect("encode captured summary");
    let script = fixture.path().join(format!("recoverable-agent-{seed}.sh"));
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nset -eu\n\
             printf '{{\"payload\":{{\"agent_id\":\"cli-recovery\",\"branch_id\":\"%s\",\"session_id\":\"%s\"}},\"protocol_version\":1,\"sequence\":0,\"type\":\"agent.start\"}}\\n' \"$EPOCH_BRANCH_ID\" \"$EPOCH_SESSION_ID\"\n\
             printf '%s\\n' {}\n\
             printf '%s\\n' {}\n\
             printf '%s\\n' {}\n\
             printf '%s\\n' {} >&2\n",
            shell_quote(&format!(
                "{{\"payload\":{{\"context_hash\":\"{state_hash}\",\"revision\":1}},\"protocol_version\":1,\"sequence\":1,\"type\":\"context.update\"}}"
            )),
            shell_quote(&format!(
                "{{\"payload\":{{\"context_hash\":\"{state_hash}\",\"safe_point_id\":\"{safe_point_id}\"}},\"protocol_version\":1,\"sequence\":2,\"type\":\"safe_point\"}}"
            )),
            shell_quote(&format!(
                "{{\"payload\":{{\"outcome\":\"succeeded\",\"output_hash\":\"{state_hash}\"}},\"protocol_version\":1,\"sequence\":3,\"type\":\"agent.completion\"}}"
            )),
            shell_quote(&summary),
        ),
    )
    .expect("write recoverable agent");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&script, permissions).expect("make script executable");
    let workspace = fixture.path().join(format!("workspace-{seed}"));
    fs::create_dir_all(workspace.join("nested/empty")).expect("create declared workspace");
    fs::write(
        workspace.join("answer.txt"),
        format!("checkpoint payload {seed}\n"),
    )
    .expect("write workspace text");
    fs::write(
        workspace.join("nested/data.bin"),
        [
            0,
            1,
            2,
            0xff,
            u8::try_from(seed).expect("fixture seed fits one byte"),
        ],
    )
    .expect("write workspace binary");
    symlink("../answer.txt", workspace.join("nested/answer-link"))
        .expect("write workspace symlink");
    let manifest = fixture.path().join(format!("recoverable-{seed}.toml"));
    fs::write(
        &manifest,
        format!(
            "schema_version = 1\nname = \"epoch-test-agent\"\nexecutable = \"{}\"\nworking_directory = \"{}\"\n",
            script.display(),
            workspace.display()
        ),
    )
    .expect("write recoverable manifest");
    (manifest, workspace)
}

fn epoch(fixture: &TempDir, arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(arguments)
        .output()
        .expect("invoke epoch CLI")
}

fn successful_json(output: &std::process::Output, operation: &str) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("{operation} did not return JSON: {error}");
    })
}

fn run_fixture(fixture: &TempDir, seed: u64) -> (String, String, PathBuf) {
    let (manifest, workspace) = fixture_manifest(fixture, seed);
    let run = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(["run", "--manifest"])
        .arg(manifest)
        .output()
        .expect("run fixture");
    let run = successful_json(&run, "run");
    (
        run["session_id"].as_str().expect("session ID").to_owned(),
        run["branch_id"].as_str().expect("branch ID").to_owned(),
        workspace,
    )
}

fn assert_pre_checkpoint_inspection(fixture: &TempDir, session: &str, branch: &str) {
    let status = successful_json(&epoch(fixture, &["status", session]), "status");
    assert_eq!(status["session_id"], session);
    assert_eq!(status["state"], "completed");
    assert_eq!(status["application"]["outcome"], "supported");
    assert!(
        status["application"]["result"]["current_epoch_id"].is_null(),
        "a completed run must not pretend that a checkpoint already exists"
    );

    let events = successful_json(
        &epoch(
            fixture,
            &["events", session, "--branch", branch, "--limit", "100"],
        ),
        "events",
    );
    assert!(
        events["events"]
            .as_array()
            .expect("events array")
            .iter()
            .any(|event| event["kind"] == "process.manifest"),
        "fresh-process inspection must retain the Week 1 process manifest"
    );
}

fn checkpoint(fixture: &TempDir, session: &str, branch: &str) -> String {
    let checkpoint = successful_json(
        &epoch(
            fixture,
            &[
                "checkpoint",
                session,
                "--branch",
                branch,
                "--label",
                "cli-cycle",
            ],
        ),
        "checkpoint",
    );
    assert_eq!(checkpoint["operation"], "checkpoint");
    assert_eq!(checkpoint["outcome"], "supported");
    assert_eq!(checkpoint["result"]["session_id"], session);
    assert_eq!(checkpoint["result"]["branch_id"], branch);
    assert_eq!(checkpoint["result"]["boundary_sequence"], 2);
    assert_eq!(
        checkpoint["result"]["restore_scope"],
        "application_and_workspace"
    );
    assert_eq!(checkpoint["result"]["process_checkpointed"], false);
    assert_eq!(
        checkpoint["result"]["workspace"]["backend"],
        "full-copy-cas-v1"
    );
    assert!(checkpoint["result"]["workspace"]["manifest_hash"].is_string());
    checkpoint["result"]["epoch_id"]
        .as_str()
        .expect("epoch ID")
        .to_owned()
}

fn restore_and_inspect(
    fixture: &TempDir,
    session: &str,
    epoch_id: &str,
    source: &Path,
    target: &Path,
) {
    fs::write(source.join("answer.txt"), b"mutated after checkpoint\n")
        .expect("mutate source text");
    fs::remove_file(source.join("nested/data.bin")).expect("remove source binary");
    fs::write(source.join("not-in-checkpoint.txt"), b"later file")
        .expect("add source file after checkpoint");

    let restore = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(["restore", epoch_id, "--workspace-target"])
        .arg(target)
        .output()
        .expect("restore composite epoch");
    let restore = successful_json(&restore, "restore");
    assert_eq!(restore["operation"], "restore");
    assert_eq!(restore["outcome"], "supported");
    assert_eq!(restore["result"]["activated"], true);
    assert_eq!(restore["result"]["process_restored"], false);
    assert_eq!(restore["result"]["workspace_restored"], true);
    assert_eq!(
        restore["result"]["workspace_target"],
        target.display().to_string()
    );
    assert_eq!(
        fs::read(target.join("answer.txt")).expect("restored workspace text"),
        fs::read(source.join("answer.txt")).map_or_else(
            |_| unreachable!(),
            |_| format!(
                "checkpoint payload {}\n",
                source
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .trim_start_matches("workspace-")
            )
            .into_bytes()
        )
    );
    assert_eq!(
        fs::read(target.join("nested/data.bin")).expect("restored workspace binary")[..4],
        [0, 1, 2, 0xff]
    );
    assert!(target.join("nested/empty").is_dir());
    assert_eq!(
        fs::read_link(target.join("nested/answer-link")).expect("restored symlink"),
        Path::new("../answer.txt")
    );
    assert!(!target.join("not-in-checkpoint.txt").exists());

    let no_clobber = Command::new(env!("CARGO_BIN_EXE_epoch"))
        .current_dir(fixture.path())
        .args(["restore", epoch_id, "--workspace-target"])
        .arg(target)
        .output()
        .expect("attempt no-clobber restore");
    assert!(!no_clobber.status.success());
    let no_clobber: serde_json::Value =
        serde_json::from_slice(&no_clobber.stdout).expect("no-clobber JSON");
    assert_eq!(no_clobber["outcome"], "failed");
    assert_eq!(no_clobber["issue"]["code"], "target_exists");

    let status = successful_json(&epoch(fixture, &["status", session]), "status");
    assert_eq!(status["session_id"], session);
    assert_eq!(status["state"], "completed");
    assert_eq!(status["application"]["outcome"], "supported");
    assert_eq!(
        status["application"]["result"]["current_epoch_id"],
        epoch_id
    );
    assert_eq!(
        status["application"]["result"]["context"]["cursors"]["boundary_sequence"],
        2
    );
}

fn assert_identical_diff(fixture: &TempDir, epoch_id: &str) {
    let diff = successful_json(
        &epoch(fixture, &["diff", epoch_id, epoch_id, "--json"]),
        "diff",
    );
    assert_eq!(diff["operation"], "diff");
    assert_eq!(diff["outcome"], "supported");
    assert_eq!(diff["result"]["before_epoch_id"], epoch_id);
    assert_eq!(diff["result"]["after_epoch_id"], epoch_id);
    assert_eq!(diff["result"]["diff"]["identical"], true);
    assert_eq!(diff["result"]["workspace"]["identical"], true);
    assert_eq!(
        diff["result"]["diff"]["unsupported_sections"][0]["section"],
        "capabilities"
    );
}

#[test]
fn cli_week_two_flow_is_restart_safe_across_three_repetitions() {
    let fixture = TempDir::new().expect("create CLI recovery fixture");
    let mut sessions = std::collections::BTreeSet::new();

    for seed in [73_u64, 74, 75] {
        let (session, branch, workspace) = run_fixture(&fixture, seed);
        assert!(
            sessions.insert(session.clone()),
            "session IDs must be fresh"
        );
        assert_pre_checkpoint_inspection(&fixture, &session, &branch);
        let epoch_id = checkpoint(&fixture, &session, &branch);
        let restore_target = fixture.path().join(format!("restored-{seed}"));
        restore_and_inspect(&fixture, &session, &epoch_id, &workspace, &restore_target);
        assert_identical_diff(&fixture, &epoch_id);
    }

    assert_eq!(sessions.len(), 3);
}

#[test]
fn cli_returns_explicit_machine_readable_failed_and_unsupported_outcomes() {
    let fixture = TempDir::new().expect("create CLI outcome fixture");
    let invalid = epoch(&fixture, &["restore", "not-an-epoch-id"]);
    assert!(!invalid.status.success());
    let invalid: serde_json::Value =
        serde_json::from_slice(&invalid.stdout).expect("invalid-ID JSON");
    assert_eq!(invalid["outcome"], "failed");
    assert_eq!(invalid["issue"]["code"], "not_found");

    let future_mode = epoch(
        &fixture,
        &[
            "restore",
            "00000000-0000-0000-0000-000000000001",
            "--mode",
            "fork-on-divergence",
        ],
    );
    assert!(!future_mode.status.success());
    let future_mode: serde_json::Value =
        serde_json::from_slice(&future_mode.stdout).expect("unsupported-mode JSON");
    assert_eq!(future_mode["outcome"], "unsupported");
    assert_eq!(future_mode["issue"]["code"], "unsupported_mode");
}

#[test]
fn cli_fork_and_branch_inspection_are_restart_safe_and_promotion_is_explicit() {
    let fixture = TempDir::new().expect("create CLI fork fixture");
    let (session, branch, _) = run_fixture(&fixture, 91);
    let epoch_id = checkpoint(&fixture, &session, &branch);

    let fork = successful_json(
        &epoch(&fixture, &["fork", &epoch_id, "--name", "cli-experiment"]),
        "fork",
    );
    assert_eq!(fork["operation"], "fork");
    assert_eq!(fork["outcome"], "supported");
    assert_eq!(fork["result"]["session_id"], session);
    assert_eq!(fork["result"]["parent_branch_id"], branch);
    assert_eq!(fork["result"]["fork_epoch_id"], epoch_id);
    assert_eq!(fork["result"]["name"], "cli-experiment");
    assert_eq!(
        fork["result"]["replay"]["continuation"]["outcome"],
        "unsupported"
    );
    assert_eq!(fork["result"]["effect_frontier"]["outcome"], "unsupported");
    let child = fork["result"]["branch_id"].as_str().expect("child branch");

    let inspect = successful_json(
        &epoch(&fixture, &["branch", "inspect", child]),
        "branch inspect",
    );
    assert_eq!(inspect["operation"], "branch.inspect");
    assert_eq!(inspect["outcome"], "supported");
    assert_eq!(inspect["result"], fork["result"]);

    let promotion = epoch(&fixture, &["branch", "promote", child]);
    assert_eq!(promotion.status.code(), Some(3));
    let promotion: serde_json::Value =
        serde_json::from_slice(&promotion.stdout).expect("promotion JSON");
    assert_eq!(promotion["operation"], "branch.promote");
    assert_eq!(promotion["outcome"], "unsupported");
    assert_eq!(promotion["issue"]["code"], "unsupported_mode");
    assert!(
        promotion["issue"]["detail"]
            .as_str()
            .expect("promotion detail")
            .contains("compare-and-swap")
    );
}

#[test]
fn trusted_security_state_survives_restore_and_fork_three_times() {
    let fixture = TempDir::new().expect("create Week 3 security fixture");

    for seed in [111_u64, 112, 113] {
        let (raw_session, raw_branch, _) = run_fixture(&fixture, seed);
        let session = SessionId::from_str(&raw_session).expect("session ID");
        let branch = BranchId::from_str(&raw_branch).expect("branch ID");
        let constraints = json!({
            "subject": "agent-1",
            "resource": "mailbox:test",
            "max_uses": 1,
            "budget_units": 1
        })
        .to_string();
        let grant = |label: &str| {
            let output = successful_json(
                &epoch(
                    &fixture,
                    &[
                        "capability",
                        "grant",
                        &raw_branch,
                        "email.send",
                        &constraints,
                    ],
                ),
                label,
            );
            (
                output["capability_id"]
                    .as_str()
                    .expect("capability ID")
                    .to_owned(),
                CapabilityHandle::from_str(output["handle"].as_str().expect("capability handle"))
                    .expect("opaque handle"),
            )
        };
        let (revoked_id, revoked_handle) = grant("grant revocable capability");
        let (_, consumed_handle) = grant("grant consumable capability");
        let before_epoch = checkpoint(&fixture, &raw_session, &raw_branch);

        let service = Arc::new(
            CapabilityService::open(fixture.path().join(".epoch/state.db"))
                .expect("capability service"),
        );
        let gateway = EffectGateway::open(
            fixture.path().join(".epoch/state.db"),
            fixture.path().join(".epoch/blobs"),
            Arc::new(
                CapabilityAuthorizer::new(service.clone(), consumed_handle.clone(), "agent-1", 1)
                    .expect("effect authority"),
            ),
            Arc::new(DeterministicLocalDispatcher::default()),
        )
        .expect("effect gateway");
        let intent = CanonicalIntent::new(
            session,
            branch,
            format!("security-{seed}/email-1"),
            "email.send",
            "mailbox:test",
            json!({"to": "checkpoint@example.test"}),
            0,
        )
        .expect("effect intent");
        gateway
            .execute(&intent, FaultPoint::None)
            .expect("consume one-use authority");
        successful_json(
            &epoch(&fixture, &["capability", "revoke", &revoked_id]),
            "revoke capability",
        );

        let restore_target = fixture.path().join(format!("security-restore-{seed}"));
        let restore = Command::new(env!("CARGO_BIN_EXE_epoch"))
            .current_dir(fixture.path())
            .args(["restore", &before_epoch, "--workspace-target"])
            .arg(&restore_target)
            .output()
            .expect("restore source epoch");
        successful_json(&restore, "security restore");

        for (handle, request_id, expected) in [
            (&revoked_handle, "restored-revoked", DenialReason::Revoked),
            (
                &consumed_handle,
                "restored-consumed",
                DenialReason::Consumed,
            ),
        ] {
            let request = CapabilityUse::new(
                session,
                branch,
                "agent-1",
                "email.send",
                "mailbox:test",
                0,
                1,
                format!("{request_id}-{seed}"),
                &"a".repeat(64),
            )
            .expect("use request");
            let decision = service
                .authorize_and_consume(handle, &request)
                .expect("durable restored denial");
            assert_eq!(decision.outcome, DecisionOutcome::Deny);
            assert_eq!(decision.reason, expected);
        }
        assert_eq!(
            gateway.list(session, None).expect("effect history").len(),
            1
        );

        let fork = successful_json(
            &epoch(
                &fixture,
                &[
                    "fork",
                    &before_epoch,
                    "--name",
                    &format!("security-fork-{seed}"),
                ],
            ),
            "security fork",
        );
        assert_eq!(
            fork["result"]["replay"]["continuation"]["outcome"],
            "unsupported"
        );
        assert_eq!(
            gateway
                .list(session, None)
                .expect("post-fork effects")
                .len(),
            1
        );

        let after_epoch = checkpoint(&fixture, &raw_session, &raw_branch);
        let diff = successful_json(
            &epoch(&fixture, &["diff", &before_epoch, &after_epoch, "--json"]),
            "security diff",
        );
        assert_eq!(
            diff["result"]["capabilities"]["changed_between_epochs"],
            true
        );
        assert_eq!(diff["result"]["effects"]["changed_between_epochs"], true);
        assert!(
            diff["result"]["capabilities"]["after_frontier"]
                .as_u64()
                .expect("after capability frontier")
                > diff["result"]["capabilities"]["before_frontier"]
                    .as_u64()
                    .expect("before capability frontier")
        );
        assert!(
            diff["result"]["effects"]["after_frontier"]
                .as_u64()
                .expect("after effect frontier")
                > diff["result"]["effects"]["before_frontier"]
                    .as_u64()
                    .expect("before effect frontier")
        );
    }
}
