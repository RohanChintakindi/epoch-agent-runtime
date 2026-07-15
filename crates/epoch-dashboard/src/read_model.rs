use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use epoch_storage::LATEST_SCHEMA_VERSION;
use rusqlite::{Connection, OpenFlags, OptionalExtension as _, params};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

pub const MAX_TIMELINE_PAGE: usize = 200;
pub const MAX_STANDARD_PAGE: usize = 100;
const MAX_BRANCHES_PER_SESSION: usize = 512;
const MAX_COMPONENTS_PER_EPOCH: usize = 64;
const MAX_RESTORES_PER_EPOCH: usize = 16;
const MAX_DIFF_CHANGES: usize = 100;
const MAX_DIFF_BYTES: usize = 256 * 1024;
const MAX_EFFECT_ATTEMPTS: usize = 32;
const MAX_EFFECT_HISTORY: usize = 64;

#[derive(Clone, Debug)]
pub struct ReadModel {
    database: PathBuf,
}

impl ReadModel {
    pub fn open(state_root: &Path) -> Result<Self, StateError> {
        let root = state_root
            .canonicalize()
            .map_err(|_| StateError::MissingStateRoot)?;
        if !root.is_dir() {
            return Err(StateError::InvalidStateRoot);
        }
        let database = root.join("state.db");
        let metadata = database
            .symlink_metadata()
            .map_err(|_| StateError::MissingDatabase)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StateError::InvalidDatabaseFile);
        }
        let model = Self { database };
        model.validate()?;
        Ok(model)
    }

    fn connection(&self) -> Result<Connection, StateError> {
        let connection = Connection::open_with_flags(
            &self.database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(2))?;
        connection.pragma_update(None, "query_only", true)?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        Ok(connection)
    }

    fn validate(&self) -> Result<(), StateError> {
        let connection = self.connection()?;
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version != LATEST_SCHEMA_VERSION {
            return Err(StateError::UnsupportedSchema {
                found: version,
                expected: LATEST_SCHEMA_VERSION,
            });
        }
        let quick_check: String =
            connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        if quick_check != "ok" {
            return Err(StateError::IntegrityCheckFailed);
        }
        Ok(())
    }

    pub fn sessions(
        &self,
        status: Option<&str>,
        offset: u64,
        limit: usize,
    ) -> Result<SessionPage, StateError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT s.id, s.state, s.policy_revision, s.revision, \
                    s.created_at_unix_ms, s.updated_at_unix_ms, \
                    (SELECT COUNT(*) FROM branches b WHERE b.session_id = s.id), \
                    (SELECT COUNT(*) FROM epochs e WHERE e.session_id = s.id) \
             FROM sessions s \
             WHERE (?1 IS NULL OR s.state = ?1) \
             ORDER BY s.updated_at_unix_ms DESC, s.id ASC \
             LIMIT ?2 OFFSET ?3",
        )?;
        let mut rows =
            statement.query(params![status, sql_page_limit(limit)?, sql_offset(offset)?])?;
        let mut items = Vec::new();
        while let Some(row) = rows.next()? {
            items.push(session_summary(row)?);
        }
        let page = finish_page(&mut items, offset, limit);
        Ok(SessionPage { items, page })
    }

    pub fn session(&self, session_id: &str) -> Result<SessionDetail, StateError> {
        require_uuid(session_id)?;
        let connection = self.connection()?;
        let session = connection
            .query_row(
                "SELECT s.id, s.state, s.policy_revision, s.revision, \
                        s.created_at_unix_ms, s.updated_at_unix_ms, \
                        (SELECT COUNT(*) FROM branches b WHERE b.session_id = s.id), \
                        (SELECT COUNT(*) FROM epochs e WHERE e.session_id = s.id) \
                 FROM sessions s WHERE s.id = ?1",
                [session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StateError::NotFound("session"))?;
        let session = SessionSummary {
            session_id: checked_uuid(session.0, "session.id")?,
            state: checked_enum(session.1, "session.state", SESSION_STATES)?,
            policy_revision: nonnegative(session.2, "session.policy_revision")?,
            revision: nonnegative(session.3, "session.revision")?,
            created_at_unix_ms: nonnegative_i64(session.4, "session.created_at")?,
            updated_at_unix_ms: nonnegative_i64(session.5, "session.updated_at")?,
            branch_count: usize_count(session.6, "session.branch_count")?,
            epoch_count: usize_count(session.7, "session.epoch_count")?,
        };

        let mut statement = connection.prepare(
            "SELECT id, parent_branch_id, fork_epoch_id, state, name, fork_point_sequence, \
                    created_at_unix_ms, updated_at_unix_ms \
             FROM branches WHERE session_id = ?1 \
             ORDER BY created_at_unix_ms ASC, id ASC \
             LIMIT ?2",
        )?;
        let mut rows =
            statement.query(params![session_id, sql_bound(MAX_BRANCHES_PER_SESSION)?])?;
        let mut branches = Vec::new();
        while let Some(row) = rows.next()? {
            let fork_point: Option<i64> = row.get(5)?;
            branches.push(BranchSummary {
                branch_id: checked_uuid(row.get(0)?, "branch.id")?,
                parent_branch_id: checked_optional_uuid(row.get(1)?, "branch.parent")?,
                fork_epoch_id: checked_optional_uuid(row.get(2)?, "branch.fork_epoch")?,
                state: checked_enum(row.get(3)?, "branch.state", BRANCH_STATES)?,
                name: checked_optional_text(row.get(4)?, "branch.name", 64)?,
                fork_point_sequence: fork_point
                    .map(|value| nonnegative(value, "branch.fork_point_sequence"))
                    .transpose()?,
                created_at_unix_ms: nonnegative_i64(row.get(6)?, "branch.created_at")?,
                updated_at_unix_ms: nonnegative_i64(row.get(7)?, "branch.updated_at")?,
            });
        }
        let branches_truncated = session.branch_count > branches.len();
        Ok(SessionDetail {
            session,
            branches,
            branches_truncated,
        })
    }

    pub fn timeline(
        &self,
        branch_id: &str,
        filters: &TimelineFilters<'_>,
        offset: u64,
        limit: usize,
    ) -> Result<TimelinePage, StateError> {
        require_uuid(branch_id)?;
        let connection = self.connection()?;
        let session_id: Option<String> = connection
            .query_row(
                "SELECT session_id FROM branches WHERE id = ?1",
                [branch_id],
                |row| row.get(0),
            )
            .optional()?;
        let session_id = checked_uuid(
            session_id.ok_or(StateError::NotFound("branch"))?,
            "branch.session_id",
        )?;
        let mut statement = connection.prepare(
            "SELECT id, sequence, epoch_id, causal_parent_id, monotonic_ns, \
                    occurred_at_unix_ms, actor, kind, status \
             FROM events \
             WHERE branch_id = ?1 \
               AND (?2 IS NULL OR actor = ?2) \
               AND (?3 IS NULL OR kind = ?3) \
               AND (?4 IS NULL OR status = ?4) \
             ORDER BY sequence ASC, id ASC \
             LIMIT ?5 OFFSET ?6",
        )?;
        let mut rows = statement.query(params![
            branch_id,
            filters.actor,
            filters.kind,
            filters.status,
            sql_page_limit(limit)?,
            sql_offset(offset)?
        ])?;
        let mut items = Vec::new();
        while let Some(row) = rows.next()? {
            items.push(TimelineEvent {
                event_id: checked_uuid(row.get(0)?, "event.id")?,
                sequence: nonnegative(row.get(1)?, "event.sequence")?,
                epoch_id: checked_optional_uuid(row.get(2)?, "event.epoch_id")?,
                causal_parent_id: checked_optional_uuid(row.get(3)?, "event.causal_parent")?,
                monotonic_ns: nonnegative(row.get(4)?, "event.monotonic_ns")?,
                occurred_at_unix_ms: nonnegative_i64(row.get(5)?, "event.occurred_at")?,
                actor: checked_enum(row.get(6)?, "event.actor", EVENT_ACTORS)?,
                kind: checked_text(row.get(7)?, "event.kind", 128)?,
                status: checked_enum(row.get(8)?, "event.status", EVENT_STATUSES)?,
            });
        }
        let page = finish_page(&mut items, offset, limit);
        Ok(TimelinePage {
            session_id,
            branch_id: branch_id.to_owned(),
            items,
            page,
            payloads_redacted: true,
        })
    }

    pub fn epochs(
        &self,
        session_id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<EpochPage, StateError> {
        require_uuid(session_id)?;
        let connection = self.connection()?;
        ensure_session(&connection, session_id)?;
        let mut statement = connection.prepare(
            "SELECT id, branch_id, parent_epoch_id, sequence, status, backend, \
                    policy_revision, effect_frontier, capability_frontier, \
                    created_at_unix_ms, committed_at_unix_ms \
             FROM epochs WHERE session_id = ?1 \
             ORDER BY created_at_unix_ms DESC, id ASC \
             LIMIT ?2 OFFSET ?3",
        )?;
        let mut rows = statement.query(params![
            session_id,
            sql_page_limit(limit)?,
            sql_offset(offset)?
        ])?;
        let mut items = Vec::new();
        while let Some(row) = rows.next()? {
            let epoch_id = checked_uuid(row.get(0)?, "epoch.id")?;
            let components = components(&connection, &epoch_id)?;
            let component_count: usize = connection
                .query_row(
                    "SELECT COUNT(*) FROM snapshot_components WHERE epoch_id = ?1",
                    [&epoch_id],
                    |component_row| component_row.get::<_, i64>(0),
                )
                .map_err(StateError::from)
                .and_then(|count| usize_count(count, "epoch.component_count"))?;
            let restore_outcomes = restores(&connection, &epoch_id)?;
            items.push(EpochSummary {
                epoch_id,
                branch_id: checked_uuid(row.get(1)?, "epoch.branch_id")?,
                parent_epoch_id: checked_optional_uuid(row.get(2)?, "epoch.parent")?,
                sequence: nonnegative(row.get(3)?, "epoch.sequence")?,
                status: checked_enum(row.get(4)?, "epoch.status", EPOCH_STATUSES)?,
                backend: checked_optional_text(row.get(5)?, "epoch.backend", 128)?,
                policy_revision: nonnegative(row.get(6)?, "epoch.policy_revision")?,
                effect_frontier: nonnegative(row.get(7)?, "epoch.effect_frontier")?,
                capability_frontier: nonnegative(row.get(8)?, "epoch.capability_frontier")?,
                created_at_unix_ms: nonnegative_i64(row.get(9)?, "epoch.created_at")?,
                committed_at_unix_ms: row
                    .get::<_, Option<i64>>(10)?
                    .map(|value| nonnegative_i64(value, "epoch.committed_at"))
                    .transpose()?,
                components,
                components_truncated: component_count > MAX_COMPONENTS_PER_EPOCH,
                restore_outcomes,
            });
        }
        let page = finish_page(&mut items, offset, limit);
        Ok(EpochPage { items, page })
    }

    pub fn diffs(
        &self,
        session_id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<DiffPage, StateError> {
        require_uuid(session_id)?;
        let connection = self.connection()?;
        ensure_session(&connection, session_id)?;
        let mut statement = connection.prepare(
            "SELECT d.id, d.left_epoch_id, d.right_epoch_id, d.schema_version, \
                    d.digest, d.summary_json, d.created_at_unix_ms \
             FROM semantic_diffs d \
             JOIN epochs left_epoch ON left_epoch.id = d.left_epoch_id \
             JOIN epochs right_epoch ON right_epoch.id = d.right_epoch_id \
             WHERE left_epoch.session_id = ?1 AND right_epoch.session_id = ?1 \
             ORDER BY d.created_at_unix_ms DESC, d.id ASC \
             LIMIT ?2 OFFSET ?3",
        )?;
        let mut rows = statement.query(params![
            session_id,
            sql_page_limit(limit)?,
            sql_offset(offset)?
        ])?;
        let mut items = Vec::new();
        while let Some(row) = rows.next()? {
            let summary: String = row.get(5)?;
            let parsed = parse_diff_summary(&summary)?;
            items.push(SemanticDiff {
                diff_id: checked_uuid(row.get(0)?, "diff.id")?,
                left_epoch_id: checked_uuid(row.get(1)?, "diff.left_epoch")?,
                right_epoch_id: checked_uuid(row.get(2)?, "diff.right_epoch")?,
                schema_version: nonnegative(row.get(3)?, "diff.schema_version")?,
                digest: checked_text(row.get(4)?, "diff.digest", 255)?,
                created_at_unix_ms: nonnegative_i64(row.get(6)?, "diff.created_at")?,
                identical: parsed.identical,
                change_count: parsed.change_count,
                changes: parsed.changes,
                changes_truncated: parsed.changes_truncated,
                unsupported_sections: parsed.unsupported_sections,
                values_redacted: true,
            });
        }
        let page = finish_page(&mut items, offset, limit);
        Ok(DiffPage { items, page })
    }

    pub fn capabilities(&self, session_id: &str) -> Result<CapabilityView, StateError> {
        require_uuid(session_id)?;
        let connection = self.connection()?;
        ensure_session(&connection, session_id)?;
        let mut statement = connection.prepare(
            "SELECT id, branch_id, subject, action, resource, delegated_from_id, \
                    remaining_uses, remaining_budget_units, policy_revision, status, \
                    issued_at_unix_ms, expires_at_unix_ms, updated_at_unix_ms \
             FROM capabilities WHERE session_id = ?1 \
             ORDER BY issued_at_unix_ms DESC, id ASC LIMIT ?2",
        )?;
        let mut rows = statement.query(params![session_id, sql_bound(MAX_STANDARD_PAGE)?])?;
        let mut current = Vec::new();
        while let Some(row) = rows.next()? {
            current.push(CapabilitySummary {
                capability_id: checked_uuid(row.get(0)?, "capability.id")?,
                branch_id: checked_uuid(row.get(1)?, "capability.branch_id")?,
                subject: checked_text(row.get(2)?, "capability.subject", 255)?,
                action: checked_text(row.get(3)?, "capability.action", 255)?,
                resource: checked_text(row.get(4)?, "capability.resource", 2_048)?,
                delegated_from_id: checked_optional_uuid(row.get(5)?, "capability.parent")?,
                remaining_uses: optional_nonnegative(row.get(6)?, "capability.remaining_uses")?,
                remaining_budget_units: optional_nonnegative(
                    row.get(7)?,
                    "capability.remaining_budget",
                )?,
                policy_revision: nonnegative(row.get(8)?, "capability.policy_revision")?,
                state: checked_enum(row.get(9)?, "capability.status", CAPABILITY_STATES)?,
                issued_at_unix_ms: nonnegative_i64(row.get(10)?, "capability.issued_at")?,
                expires_at_unix_ms: optional_nonnegative_i64(
                    row.get(11)?,
                    "capability.expires_at",
                )?,
                updated_at_unix_ms: nonnegative_i64(row.get(12)?, "capability.updated_at")?,
            });
        }
        let current_total: usize = connection
            .query_row(
                "SELECT COUNT(*) FROM capabilities WHERE session_id = ?1",
                [session_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StateError::from)
            .and_then(|count| usize_count(count, "capability.count"))?;

        let mut statement = connection.prepare(
            "SELECT sequence, capability_id, branch_id, subject, action, resource, \
                    policy_revision, budget_units, outcome, reason, decided_at_unix_ms \
             FROM capability_decisions WHERE session_id = ?1 \
             ORDER BY sequence DESC LIMIT ?2",
        )?;
        let mut rows = statement.query(params![session_id, sql_bound(MAX_STANDARD_PAGE)?])?;
        let mut audit = Vec::new();
        while let Some(row) = rows.next()? {
            audit.push(CapabilityDecision {
                sequence: nonnegative(row.get(0)?, "capability_decision.sequence")?,
                capability_id: checked_optional_uuid(
                    row.get(1)?,
                    "capability_decision.capability",
                )?,
                branch_id: checked_uuid(row.get(2)?, "capability_decision.branch")?,
                subject: checked_text(row.get(3)?, "capability_decision.subject", 255)?,
                action: checked_text(row.get(4)?, "capability_decision.action", 255)?,
                resource: checked_text(row.get(5)?, "capability_decision.resource", 2_048)?,
                policy_revision: nonnegative(row.get(6)?, "capability_decision.policy_revision")?,
                budget_units: nonnegative(row.get(7)?, "capability_decision.budget_units")?,
                outcome: checked_enum(
                    row.get(8)?,
                    "capability_decision.outcome",
                    &["allow", "deny"],
                )?,
                reason: checked_text(row.get(9)?, "capability_decision.reason", 64)?,
                decided_at_unix_ms: nonnegative_i64(
                    row.get(10)?,
                    "capability_decision.decided_at",
                )?,
            });
        }
        let audit_total: usize = connection
            .query_row(
                "SELECT COUNT(*) FROM capability_decisions WHERE session_id = ?1",
                [session_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StateError::from)
            .and_then(|count| usize_count(count, "capability_decision.count"))?;
        Ok(CapabilityView {
            current,
            current_truncated: current_total > MAX_STANDARD_PAGE,
            audit,
            audit_truncated: audit_total > MAX_STANDARD_PAGE,
            bearer_material_exposed: false,
        })
    }

    pub fn effects(&self, session_id: &str) -> Result<EffectView, StateError> {
        require_uuid(session_id)?;
        let connection = self.connection()?;
        ensure_session(&connection, session_id)?;
        let mut statement = connection.prepare(
            "SELECT id, branch_id, capability_id, operation_id, replay_key, action, resource, \
                    state, policy_revision, prepared_at_unix_ms, dispatched_at_unix_ms, \
                    resolved_at_unix_ms, revision \
             FROM effect_intents WHERE session_id = ?1 \
             ORDER BY prepared_at_unix_ms DESC, id ASC LIMIT ?2",
        )?;
        let mut rows = statement.query(params![session_id, sql_bound(MAX_STANDARD_PAGE)?])?;
        let mut intents = Vec::new();
        while let Some(row) = rows.next()? {
            let effect_id = checked_uuid(row.get(0)?, "effect.id")?;
            intents.push(EffectSummary {
                attempts: effect_attempts(&connection, &effect_id)?,
                transitions: effect_transitions(&connection, &effect_id)?,
                effect_id,
                branch_id: checked_uuid(row.get(1)?, "effect.branch")?,
                capability_id: checked_optional_uuid(row.get(2)?, "effect.capability")?,
                operation_id: checked_text(row.get(3)?, "effect.operation_id", 255)?,
                replay_key: checked_text(row.get(4)?, "effect.replay_key", 255)?,
                action: checked_text(row.get(5)?, "effect.action", 255)?,
                resource: checked_text(row.get(6)?, "effect.resource", 2_048)?,
                state: checked_enum(row.get(7)?, "effect.state", EFFECT_STATES)?,
                policy_revision: nonnegative(row.get(8)?, "effect.policy_revision")?,
                prepared_at_unix_ms: nonnegative_i64(row.get(9)?, "effect.prepared_at")?,
                dispatched_at_unix_ms: optional_nonnegative_i64(
                    row.get(10)?,
                    "effect.dispatched_at",
                )?,
                resolved_at_unix_ms: optional_nonnegative_i64(row.get(11)?, "effect.resolved_at")?,
                revision: nonnegative(row.get(12)?, "effect.revision")?,
            });
        }
        let count: usize = connection
            .query_row(
                "SELECT COUNT(*) FROM effect_intents WHERE session_id = ?1",
                [session_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StateError::from)
            .and_then(|value| usize_count(value, "effect.count"))?;
        Ok(EffectView {
            intents,
            truncated: count > MAX_STANDARD_PAGE,
            provider_content_exposed: false,
        })
    }
}

const SESSION_STATES: &[&str] = &[
    "created",
    "starting",
    "running",
    "suspended",
    "checkpointing",
    "restoring",
    "completed",
    "failed",
];
const BRANCH_STATES: &[&str] = &[
    "created",
    "running",
    "suspended",
    "completed",
    "promoted",
    "abandoned",
    "failed",
];
const EVENT_ACTORS: &[&str] = &["agent", "supervisor", "tool", "gateway", "operator"];
const EVENT_STATUSES: &[&str] = &["started", "succeeded", "failed", "denied", "unknown"];
const EPOCH_STATUSES: &[&str] = &["creating", "committed", "failed"];
const CAPABILITY_STATES: &[&str] = &["active", "consumed", "expired", "revoked"];
const EFFECT_STATES: &[&str] = &[
    "prepared",
    "denied",
    "dispatched",
    "succeeded",
    "failed",
    "unknown",
];

fn session_summary(row: &rusqlite::Row<'_>) -> Result<SessionSummary, StateError> {
    Ok(SessionSummary {
        session_id: checked_uuid(row.get(0)?, "session.id")?,
        state: checked_enum(row.get(1)?, "session.state", SESSION_STATES)?,
        policy_revision: nonnegative(row.get(2)?, "session.policy_revision")?,
        revision: nonnegative(row.get(3)?, "session.revision")?,
        created_at_unix_ms: nonnegative_i64(row.get(4)?, "session.created_at")?,
        updated_at_unix_ms: nonnegative_i64(row.get(5)?, "session.updated_at")?,
        branch_count: usize_count(row.get(6)?, "session.branch_count")?,
        epoch_count: usize_count(row.get(7)?, "session.epoch_count")?,
    })
}

fn components(
    connection: &Connection,
    epoch_id: &str,
) -> Result<Vec<SnapshotComponent>, StateError> {
    let mut statement = connection.prepare(
        "SELECT kind, status, backend, byte_length, staged_at_unix_ms, committed_at_unix_ms \
         FROM snapshot_components WHERE epoch_id = ?1 ORDER BY kind ASC LIMIT ?2",
    )?;
    let mut rows = statement.query(params![epoch_id, sql_bound(MAX_COMPONENTS_PER_EPOCH)?])?;
    let mut components = Vec::new();
    while let Some(row) = rows.next()? {
        components.push(SnapshotComponent {
            kind: checked_text(row.get(0)?, "component.kind", 128)?,
            status: checked_enum(
                row.get(1)?,
                "component.status",
                &["staged", "committed", "failed"],
            )?,
            backend: checked_text(row.get(2)?, "component.backend", 128)?,
            byte_length: nonnegative(row.get(3)?, "component.byte_length")?,
            staged_at_unix_ms: nonnegative_i64(row.get(4)?, "component.staged_at")?,
            committed_at_unix_ms: optional_nonnegative_i64(row.get(5)?, "component.committed_at")?,
        });
    }
    Ok(components)
}

fn restores(connection: &Connection, epoch_id: &str) -> Result<Vec<RestoreOutcome>, StateError> {
    let mut statement = connection.prepare(
        "SELECT id, branch_id, status, occurred_at_unix_ms \
         FROM events WHERE epoch_id = ?1 AND kind = 'application.context_restored' \
         ORDER BY occurred_at_unix_ms DESC, id ASC LIMIT ?2",
    )?;
    let mut rows = statement.query(params![epoch_id, sql_bound(MAX_RESTORES_PER_EPOCH)?])?;
    let mut restores = Vec::new();
    while let Some(row) = rows.next()? {
        restores.push(RestoreOutcome {
            event_id: checked_uuid(row.get(0)?, "restore.event_id")?,
            branch_id: checked_uuid(row.get(1)?, "restore.branch_id")?,
            status: checked_enum(row.get(2)?, "restore.status", EVENT_STATUSES)?,
            occurred_at_unix_ms: nonnegative_i64(row.get(3)?, "restore.occurred_at")?,
        });
    }
    Ok(restores)
}

fn effect_attempts(
    connection: &Connection,
    effect_id: &str,
) -> Result<Vec<EffectAttempt>, StateError> {
    let mut statement = connection.prepare(
        "SELECT id, attempt_no, state, started_at_unix_ms, completed_at_unix_ms \
         FROM effect_attempts WHERE effect_id = ?1 ORDER BY attempt_no ASC LIMIT ?2",
    )?;
    let mut rows = statement.query(params![effect_id, sql_bound(MAX_EFFECT_ATTEMPTS)?])?;
    let mut attempts = Vec::new();
    while let Some(row) = rows.next()? {
        let attempt_no = nonnegative(row.get(1)?, "effect_attempt.number")?;
        attempts.push(EffectAttempt {
            attempt_id: checked_uuid(row.get(0)?, "effect_attempt.id")?,
            attempt_no,
            state: checked_enum(
                row.get(2)?,
                "effect_attempt.state",
                &["started", "succeeded", "failed", "unknown"],
            )?,
            started_at_unix_ms: nonnegative_i64(row.get(3)?, "effect_attempt.started_at")?,
            completed_at_unix_ms: optional_nonnegative_i64(
                row.get(4)?,
                "effect_attempt.completed_at",
            )?,
            history: effect_attempt_history(connection, effect_id, attempt_no)?,
        });
    }
    Ok(attempts)
}

fn effect_attempt_history(
    connection: &Connection,
    effect_id: &str,
    attempt_no: u64,
) -> Result<Vec<EffectHistory>, StateError> {
    let mut statement = connection.prepare(
        "SELECT sequence, state, occurred_at_unix_ms FROM effect_attempt_history \
         WHERE effect_id = ?1 AND attempt_no = ?2 ORDER BY sequence ASC LIMIT ?3",
    )?;
    let mut rows = statement.query(params![
        effect_id,
        i64::try_from(attempt_no).map_err(|_| StateError::Corrupt("effect_attempt.number"))?,
        sql_bound(MAX_EFFECT_HISTORY)?
    ])?;
    history_rows(&mut rows)
}

fn effect_transitions(
    connection: &Connection,
    effect_id: &str,
) -> Result<Vec<EffectHistory>, StateError> {
    let mut statement = connection.prepare(
        "SELECT sequence, state, occurred_at_unix_ms FROM effect_transition_history \
         WHERE effect_id = ?1 ORDER BY sequence ASC LIMIT ?2",
    )?;
    let mut rows = statement.query(params![effect_id, sql_bound(MAX_EFFECT_HISTORY)?])?;
    history_rows(&mut rows)
}

fn history_rows(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<EffectHistory>, StateError> {
    let mut history = Vec::new();
    while let Some(row) = rows.next()? {
        history.push(EffectHistory {
            sequence: nonnegative(row.get(0)?, "effect_history.sequence")?,
            state: checked_text(row.get(1)?, "effect_history.state", 32)?,
            occurred_at_unix_ms: nonnegative_i64(row.get(2)?, "effect_history.occurred_at")?,
        });
    }
    Ok(history)
}

struct ParsedDiff {
    identical: Option<bool>,
    change_count: usize,
    changes: Vec<SemanticChange>,
    changes_truncated: bool,
    unsupported_sections: Vec<String>,
}

fn parse_diff_summary(encoded: &str) -> Result<ParsedDiff, StateError> {
    if encoded.len() > MAX_DIFF_BYTES {
        return Err(StateError::Corrupt("diff.summary_size"));
    }
    let value: Value =
        serde_json::from_str(encoded).map_err(|_| StateError::Corrupt("diff.summary_json"))?;
    let object = value
        .as_object()
        .ok_or(StateError::Corrupt("diff.summary_shape"))?;
    let identical = object.get("identical").and_then(Value::as_bool);
    let source_changes = object
        .get("changes")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let change_count = source_changes.len();
    let mut changes = Vec::new();
    for change in source_changes.iter().take(MAX_DIFF_CHANGES) {
        let change = change
            .as_object()
            .ok_or(StateError::Corrupt("diff.change_shape"))?;
        changes.push(SemanticChange {
            section: checked_text(
                value_string(change.get("section"), "diff.change.section")?,
                "diff.change.section",
                64,
            )?,
            path: checked_text(
                value_string(change.get("path"), "diff.change.path")?,
                "diff.change.path",
                1_024,
            )?,
            classification: checked_text(
                value_string(change.get("classification"), "diff.change.classification")?,
                "diff.change.classification",
                32,
            )?,
        });
    }
    let unsupported_sections = object
        .get("unsupported_sections")
        .and_then(Value::as_array)
        .map(|sections| {
            sections
                .iter()
                .take(32)
                .filter_map(|section| {
                    section
                        .as_object()
                        .and_then(|value| value.get("section"))
                        .and_then(Value::as_str)
                        .and_then(|value| (value.len() <= 64).then(|| value.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ParsedDiff {
        identical,
        change_count,
        changes,
        changes_truncated: change_count > MAX_DIFF_CHANGES,
        unsupported_sections,
    })
}

fn value_string(value: Option<&Value>, field: &'static str) -> Result<String, StateError> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(StateError::Corrupt(field))
}

fn ensure_session(connection: &Connection, session_id: &str) -> Result<(), StateError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
        [session_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(StateError::NotFound("session"))
    }
}

fn sql_page_limit(limit: usize) -> Result<i64, StateError> {
    i64::try_from(limit.checked_add(1).ok_or(StateError::InvalidPage)?)
        .map_err(|_| StateError::InvalidPage)
}

fn sql_bound(limit: usize) -> Result<i64, StateError> {
    i64::try_from(limit).map_err(|_| StateError::InvalidPage)
}

fn sql_offset(offset: u64) -> Result<i64, StateError> {
    i64::try_from(offset).map_err(|_| StateError::InvalidPage)
}

fn finish_page<T>(items: &mut Vec<T>, offset: u64, limit: usize) -> Page {
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_offset =
        has_more.then(|| offset.saturating_add(u64::try_from(limit).unwrap_or(u64::MAX)));
    Page {
        offset,
        limit,
        has_more,
        next_offset,
    }
}

fn require_uuid(value: &str) -> Result<(), StateError> {
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| StateError::InvalidIdentifier)
}

fn checked_uuid(value: String, field: &'static str) -> Result<String, StateError> {
    Uuid::parse_str(&value).map_err(|_| StateError::Corrupt(field))?;
    Ok(value)
}

fn checked_optional_uuid(
    value: Option<String>,
    field: &'static str,
) -> Result<Option<String>, StateError> {
    value.map(|value| checked_uuid(value, field)).transpose()
}

fn checked_text(value: String, field: &'static str, maximum: usize) -> Result<String, StateError> {
    if value.is_empty() || value.len() > maximum || value.chars().any(char::is_control) {
        Err(StateError::Corrupt(field))
    } else {
        Ok(value)
    }
}

fn checked_optional_text(
    value: Option<String>,
    field: &'static str,
    maximum: usize,
) -> Result<Option<String>, StateError> {
    value
        .map(|value| checked_text(value, field, maximum))
        .transpose()
}

fn checked_enum(
    value: String,
    field: &'static str,
    allowed: &[&str],
) -> Result<String, StateError> {
    if allowed.contains(&value.as_str()) {
        Ok(value)
    } else {
        Err(StateError::Corrupt(field))
    }
}

fn nonnegative(value: i64, field: &'static str) -> Result<u64, StateError> {
    u64::try_from(value).map_err(|_| StateError::Corrupt(field))
}

fn optional_nonnegative(
    value: Option<i64>,
    field: &'static str,
) -> Result<Option<u64>, StateError> {
    value.map(|value| nonnegative(value, field)).transpose()
}

fn nonnegative_i64(value: i64, field: &'static str) -> Result<i64, StateError> {
    if value >= 0 {
        Ok(value)
    } else {
        Err(StateError::Corrupt(field))
    }
}

fn optional_nonnegative_i64(
    value: Option<i64>,
    field: &'static str,
) -> Result<Option<i64>, StateError> {
    value.map(|value| nonnegative_i64(value, field)).transpose()
}

fn usize_count(value: i64, field: &'static str) -> Result<usize, StateError> {
    usize::try_from(value).map_err(|_| StateError::Corrupt(field))
}

#[derive(Clone, Debug, Serialize)]
pub struct Page {
    pub offset: u64,
    pub limit: usize,
    pub has_more: bool,
    pub next_offset: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub state: String,
    pub policy_revision: u64,
    pub revision: u64,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
    pub branch_count: usize,
    pub epoch_count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionPage {
    pub items: Vec<SessionSummary>,
    pub page: Page,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionDetail {
    #[serde(flatten)]
    pub session: SessionSummary,
    pub branches: Vec<BranchSummary>,
    pub branches_truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchSummary {
    pub branch_id: String,
    pub parent_branch_id: Option<String>,
    pub fork_epoch_id: Option<String>,
    pub state: String,
    pub name: Option<String>,
    pub fork_point_sequence: Option<u64>,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

pub struct TimelineFilters<'a> {
    pub actor: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub status: Option<&'a str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TimelineEvent {
    pub event_id: String,
    pub sequence: u64,
    pub epoch_id: Option<String>,
    pub causal_parent_id: Option<String>,
    pub monotonic_ns: u64,
    pub occurred_at_unix_ms: i64,
    pub actor: String,
    pub kind: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TimelinePage {
    pub session_id: String,
    pub branch_id: String,
    pub items: Vec<TimelineEvent>,
    pub page: Page,
    pub payloads_redacted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct SnapshotComponent {
    pub kind: String,
    pub status: String,
    pub backend: String,
    pub byte_length: u64,
    pub staged_at_unix_ms: i64,
    pub committed_at_unix_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RestoreOutcome {
    pub event_id: String,
    pub branch_id: String,
    pub status: String,
    pub occurred_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct EpochSummary {
    pub epoch_id: String,
    pub branch_id: String,
    pub parent_epoch_id: Option<String>,
    pub sequence: u64,
    pub status: String,
    pub backend: Option<String>,
    pub policy_revision: u64,
    pub effect_frontier: u64,
    pub capability_frontier: u64,
    pub created_at_unix_ms: i64,
    pub committed_at_unix_ms: Option<i64>,
    pub components: Vec<SnapshotComponent>,
    pub components_truncated: bool,
    pub restore_outcomes: Vec<RestoreOutcome>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EpochPage {
    pub items: Vec<EpochSummary>,
    pub page: Page,
}

#[derive(Clone, Debug, Serialize)]
pub struct SemanticChange {
    pub section: String,
    pub path: String,
    pub classification: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SemanticDiff {
    pub diff_id: String,
    pub left_epoch_id: String,
    pub right_epoch_id: String,
    pub schema_version: u64,
    pub digest: String,
    pub created_at_unix_ms: i64,
    pub identical: Option<bool>,
    pub change_count: usize,
    pub changes: Vec<SemanticChange>,
    pub changes_truncated: bool,
    pub unsupported_sections: Vec<String>,
    pub values_redacted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiffPage {
    pub items: Vec<SemanticDiff>,
    pub page: Page,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapabilitySummary {
    pub capability_id: String,
    pub branch_id: String,
    pub subject: String,
    pub action: String,
    pub resource: String,
    pub delegated_from_id: Option<String>,
    pub remaining_uses: Option<u64>,
    pub remaining_budget_units: Option<u64>,
    pub policy_revision: u64,
    pub state: String,
    pub issued_at_unix_ms: i64,
    pub expires_at_unix_ms: Option<i64>,
    pub updated_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapabilityDecision {
    pub sequence: u64,
    pub capability_id: Option<String>,
    pub branch_id: String,
    pub subject: String,
    pub action: String,
    pub resource: String,
    pub policy_revision: u64,
    pub budget_units: u64,
    pub outcome: String,
    pub reason: String,
    pub decided_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapabilityView {
    pub current: Vec<CapabilitySummary>,
    pub current_truncated: bool,
    pub audit: Vec<CapabilityDecision>,
    pub audit_truncated: bool,
    pub bearer_material_exposed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectHistory {
    pub sequence: u64,
    pub state: String,
    pub occurred_at_unix_ms: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectAttempt {
    pub attempt_id: String,
    pub attempt_no: u64,
    pub state: String,
    pub started_at_unix_ms: i64,
    pub completed_at_unix_ms: Option<i64>,
    pub history: Vec<EffectHistory>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectSummary {
    pub effect_id: String,
    pub branch_id: String,
    pub capability_id: Option<String>,
    pub operation_id: String,
    pub replay_key: String,
    pub action: String,
    pub resource: String,
    pub state: String,
    pub policy_revision: u64,
    pub prepared_at_unix_ms: i64,
    pub dispatched_at_unix_ms: Option<i64>,
    pub resolved_at_unix_ms: Option<i64>,
    pub revision: u64,
    pub attempts: Vec<EffectAttempt>,
    pub transitions: Vec<EffectHistory>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectView {
    pub intents: Vec<EffectSummary>,
    pub truncated: bool,
    pub provider_content_exposed: bool,
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("Epoch state root does not exist")]
    MissingStateRoot,
    #[error("Epoch state root is not a directory")]
    InvalidStateRoot,
    #[error("Epoch state database does not exist")]
    MissingDatabase,
    #[error("Epoch state database must be a regular non-symlink file")]
    InvalidDatabaseFile,
    #[error("Epoch schema version {found} does not match expected version {expected}")]
    UnsupportedSchema { found: i64, expected: i64 },
    #[error("Epoch state database failed its integrity check")]
    IntegrityCheckFailed,
    #[error("requested {0} does not exist")]
    NotFound(&'static str),
    #[error("invalid identifier")]
    InvalidIdentifier,
    #[error("invalid page")]
    InvalidPage,
    #[error("trusted state contains an invalid {0}")]
    Corrupt(&'static str),
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
}
