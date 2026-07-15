use std::{
    str::FromStr,
    sync::{
        Arc, Barrier,
        atomic::{AtomicI64, Ordering},
    },
    thread,
};

use epoch_capabilities::{
    CapabilityConstraints, CapabilityDecision, CapabilityHandle, CapabilityService, CapabilityUse,
    Clock, DecisionOutcome, DenialReason, IssueRequest,
};
use epoch_core::{BranchId, SessionId};
use epoch_storage::Store;
use rusqlite::params;
use tempfile::TempDir;

#[derive(Debug)]
struct ManualClock(AtomicI64);

impl ManualClock {
    fn new(now: i64) -> Self {
        Self(AtomicI64::new(now))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_unix_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Fixture {
    _directory: TempDir,
    database: std::path::PathBuf,
    session: SessionId,
    branch: BranchId,
    clock: Arc<ManualClock>,
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new().expect("temporary runtime");
        let database = directory.path().join("state.db");
        let session = SessionId::new();
        let branch = BranchId::new();
        let store = Store::open(&database).expect("open store");
        store
            .connection()
            .execute(
                "INSERT INTO sessions (id, state, created_at_unix_ms, updated_at_unix_ms) \
             VALUES (?1, 'running', 1000, 1000)",
                [session.to_string()],
            )
            .expect("session");
        store.connection().execute(
            "INSERT INTO branches (id, session_id, state, created_at_unix_ms, updated_at_unix_ms) \
             VALUES (?1, ?2, 'running', 1000, 1000)",
            params![branch.to_string(), session.to_string()],
        ).expect("branch");
        Self {
            _directory: directory,
            database,
            session,
            branch,
            clock: Arc::new(ManualClock::new(1_000)),
        }
    }

    fn service(&self) -> CapabilityService {
        CapabilityService::open_with_clock(&self.database, self.clock.clone()).expect("service")
    }

    fn issue_request(&self) -> IssueRequest {
        IssueRequest {
            session_id: self.session,
            branch_id: self.branch,
            subject: "agent-1".to_owned(),
            action: "email.send".to_owned(),
            resource: "mailbox:test".to_owned(),
            constraints: CapabilityConstraints {
                max_uses: Some(2),
                budget_units: Some(4),
            },
            expires_at_unix_ms: Some(2_000),
            policy_revision: 7,
        }
    }

    fn use_request(&self, request_id: &str) -> CapabilityUse {
        CapabilityUse::new(
            self.session,
            self.branch,
            "agent-1",
            "email.send",
            "mailbox:test",
            7,
            1,
            request_id,
            &"a".repeat(64),
        )
        .expect("valid use")
    }

    fn initialize(&self, service: &CapabilityService) {
        service
            .set_policy_revision(self.session, self.branch, 7)
            .expect("policy");
    }
}

fn assert_denied(decision: &CapabilityDecision, reason: DenialReason) {
    assert_eq!(decision.outcome, DecisionOutcome::Deny);
    assert_eq!(decision.reason, reason);
}

#[test]
fn opaque_handle_is_not_authoritative_plaintext_at_rest() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.initialize(&service);
    let issued = service.issue(&fixture.issue_request()).expect("issue");

    let bytes = std::fs::read(&fixture.database).expect("database bytes");
    assert!(
        !bytes
            .windows(issued.handle.expose().len())
            .any(|window| { window == issued.handle.expose().as_bytes() })
    );
    assert!(!format!("{:?}", issued.handle).contains(issued.handle.expose()));
    assert!(CapabilityHandle::from_str(issued.handle.expose()).is_ok());
}

#[test]
fn authorization_is_exactly_bound_and_every_decision_is_audited() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.initialize(&service);
    let issued = service.issue(&fixture.issue_request()).expect("issue");

    let allowed = service
        .authorize_and_consume(&issued.handle, &fixture.use_request("op-1"))
        .expect("allow decision");
    assert_eq!(allowed.outcome, DecisionOutcome::Allow);

    let other_session = SessionId::new();
    let wrong_session = CapabilityUse::new(
        other_session,
        fixture.branch,
        "agent-1",
        "email.send",
        "mailbox:test",
        7,
        1,
        "op-2",
        &"b".repeat(64),
    )
    .expect("request");
    assert_denied(
        &service
            .authorize_and_consume(&issued.handle, &wrong_session)
            .expect("deny"),
        DenialReason::SessionMismatch,
    );

    let other_branch = BranchId::new();
    let wrong_branch = CapabilityUse::new(
        fixture.session,
        other_branch,
        "agent-1",
        "email.send",
        "mailbox:test",
        7,
        1,
        "op-3",
        &"c".repeat(64),
    )
    .expect("request");
    assert_denied(
        &service
            .authorize_and_consume(&issued.handle, &wrong_branch)
            .expect("deny"),
        DenialReason::BranchMismatch,
    );

    for (request, reason) in [
        (
            CapabilityUse::new(
                fixture.session,
                fixture.branch,
                "agent-2",
                "email.send",
                "mailbox:test",
                7,
                1,
                "op-4",
                &"d".repeat(64),
            )
            .expect("request"),
            DenialReason::SubjectMismatch,
        ),
        (
            CapabilityUse::new(
                fixture.session,
                fixture.branch,
                "agent-1",
                "email.read",
                "mailbox:test",
                7,
                1,
                "op-5",
                &"e".repeat(64),
            )
            .expect("request"),
            DenialReason::ActionMismatch,
        ),
        (
            CapabilityUse::new(
                fixture.session,
                fixture.branch,
                "agent-1",
                "email.send",
                "mailbox:other",
                7,
                1,
                "op-6",
                &"f".repeat(64),
            )
            .expect("request"),
            DenialReason::ResourceMismatch,
        ),
    ] {
        assert_denied(
            &service
                .authorize_and_consume(&issued.handle, &request)
                .expect("deny"),
            reason,
        );
    }

    assert_eq!(service.audit_history().expect("audit").len(), 6);
}

#[test]
fn expiry_revocation_and_policy_change_defeat_restored_stale_handles() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.initialize(&service);

    let expiring = service.issue(&fixture.issue_request()).expect("issue");
    fixture.clock.set(2_001);
    assert_denied(
        &service
            .authorize_and_consume(&expiring.handle, &fixture.use_request("expired"))
            .expect("decision"),
        DenialReason::Expired,
    );

    fixture.clock.set(1_100);
    let revoked = service.issue(&fixture.issue_request()).expect("issue");
    service.revoke(&revoked.handle).expect("revoke");
    assert_denied(
        &service
            .authorize_and_consume(&revoked.handle, &fixture.use_request("revoked"))
            .expect("decision"),
        DenialReason::Revoked,
    );

    let stale = service.issue(&fixture.issue_request()).expect("issue");
    service
        .set_policy_revision(fixture.session, fixture.branch, 8)
        .expect("advance policy");
    assert_denied(
        &service
            .authorize_and_consume(&stale.handle, &fixture.use_request("stale"))
            .expect("decision"),
        DenialReason::PolicyStale,
    );
}

#[test]
fn attenuation_can_only_narrow_and_shares_ancestor_counters() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.initialize(&service);
    let root = service.issue(&fixture.issue_request()).expect("root");

    let mut widened = fixture.issue_request();
    widened.constraints.max_uses = Some(3);
    assert!(service.attenuate(&root.handle, &widened).is_err());

    let mut child_request = fixture.issue_request();
    child_request.constraints.max_uses = Some(2);
    child_request.constraints.budget_units = Some(2);
    child_request.expires_at_unix_ms = Some(1_900);
    let child = service
        .attenuate(&root.handle, &child_request)
        .expect("child");

    assert_eq!(
        service
            .authorize_and_consume(&child.handle, &fixture.use_request("child-1"))
            .expect("first")
            .outcome,
        DecisionOutcome::Allow,
    );
    assert_eq!(
        service
            .authorize_and_consume(&root.handle, &fixture.use_request("root-1"))
            .expect("second")
            .outcome,
        DecisionOutcome::Allow,
    );
    assert_denied(
        &service
            .authorize_and_consume(&child.handle, &fixture.use_request("child-2"))
            .expect("root exhausted"),
        DenialReason::AncestorConsumed,
    );
}

#[test]
fn budget_is_transactionally_enforced() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.initialize(&service);
    let mut request = fixture.issue_request();
    request.constraints.max_uses = None;
    request.constraints.budget_units = Some(3);
    let issued = service.issue(&request).expect("issue");

    let two_units = CapabilityUse::new(
        fixture.session,
        fixture.branch,
        "agent-1",
        "email.send",
        "mailbox:test",
        7,
        2,
        "budget-1",
        &"1".repeat(64),
    )
    .expect("request");
    let another_two_units = CapabilityUse::new(
        fixture.session,
        fixture.branch,
        "agent-1",
        "email.send",
        "mailbox:test",
        7,
        2,
        "budget-2",
        &"2".repeat(64),
    )
    .expect("request");
    assert_eq!(
        service
            .authorize_and_consume(&issued.handle, &two_units)
            .expect("allow")
            .outcome,
        DecisionOutcome::Allow
    );
    assert_denied(
        &service
            .authorize_and_consume(&issued.handle, &another_two_units)
            .expect("deny"),
        DenialReason::BudgetExceeded,
    );
}

#[test]
fn concurrent_one_use_authorization_has_exactly_one_winner() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.initialize(&service);
    let mut request = fixture.issue_request();
    request.constraints.max_uses = Some(1);
    request.constraints.budget_units = None;
    let issued = service.issue(&request).expect("issue");
    let barrier = Arc::new(Barrier::new(9));

    let threads = (0..8)
        .map(|index| {
            let database = fixture.database.clone();
            let clock = fixture.clock.clone();
            let handle = issued.handle.clone();
            let barrier = barrier.clone();
            let request = CapabilityUse::new(
                fixture.session,
                fixture.branch,
                "agent-1",
                "email.send",
                "mailbox:test",
                7,
                1,
                format!("concurrent-{index}"),
                &format!("{index:064x}"),
            )
            .expect("request");
            thread::spawn(move || {
                let service = CapabilityService::open_with_clock(database, clock).expect("service");
                barrier.wait();
                service
                    .authorize_and_consume(&handle, &request)
                    .expect("decision")
                    .outcome
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let outcomes = threads
        .into_iter()
        .map(|thread| thread.join().expect("thread"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == DecisionOutcome::Allow)
            .count(),
        1
    );
    assert_eq!(service.audit_history().expect("audit").len(), 8);
}

#[test]
fn unknown_well_formed_handle_is_denied_and_audited() {
    let fixture = Fixture::new();
    let service = fixture.service();
    fixture.initialize(&service);
    let unknown = CapabilityHandle::from_str(&format!("ecap_v1_{}", "0".repeat(64)))
        .expect("syntactically valid");
    assert_denied(
        &service
            .authorize_and_consume(&unknown, &fixture.use_request("unknown"))
            .expect("deny"),
        DenialReason::UnknownHandle,
    );
    assert_eq!(service.audit_history().expect("audit").len(), 1);
}
