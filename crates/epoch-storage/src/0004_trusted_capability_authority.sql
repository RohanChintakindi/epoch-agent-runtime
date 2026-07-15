ALTER TABLE capabilities ADD COLUMN remaining_budget_units INTEGER
    CHECK (remaining_budget_units IS NULL OR remaining_budget_units >= 0);

CREATE TABLE capability_policy_revisions (
    session_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    current_revision INTEGER NOT NULL CHECK (current_revision >= 0),
    updated_at_unix_ms INTEGER NOT NULL CHECK (updated_at_unix_ms >= 0),
    PRIMARY KEY (session_id, branch_id),
    FOREIGN KEY (branch_id, session_id)
        REFERENCES branches(id, session_id) ON DELETE NO ACTION
) STRICT;

CREATE TABLE capability_ancestry (
    capability_id TEXT NOT NULL,
    ancestor_id TEXT NOT NULL,
    depth INTEGER NOT NULL CHECK (depth >= 0),
    PRIMARY KEY (capability_id, ancestor_id),
    UNIQUE (capability_id, depth),
    FOREIGN KEY (capability_id) REFERENCES capabilities(id) ON DELETE NO ACTION,
    FOREIGN KEY (ancestor_id) REFERENCES capabilities(id) ON DELETE NO ACTION
) STRICT;

CREATE TABLE capability_authorizations (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    capability_id TEXT NOT NULL,
    request_id TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 255),
    request_hash TEXT NOT NULL CHECK (
        length(request_hash) = 64 AND request_hash NOT GLOB '*[^0-9a-f]*'
    ),
    budget_units INTEGER NOT NULL CHECK (budget_units >= 1),
    authorized_at_unix_ms INTEGER NOT NULL CHECK (authorized_at_unix_ms >= 0),
    UNIQUE (capability_id, request_id),
    FOREIGN KEY (capability_id) REFERENCES capabilities(id) ON DELETE NO ACTION
) STRICT;

CREATE TABLE capability_decisions (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    decision_id TEXT NOT NULL UNIQUE,
    capability_id TEXT,
    handle_hash TEXT NOT NULL CHECK (
        length(handle_hash) = 64 AND handle_hash NOT GLOB '*[^0-9a-f]*'
    ),
    session_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    subject TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 255),
    action TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 255),
    resource TEXT NOT NULL CHECK (length(resource) BETWEEN 1 AND 2048),
    request_id TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 255),
    request_hash TEXT NOT NULL CHECK (
        length(request_hash) = 64 AND request_hash NOT GLOB '*[^0-9a-f]*'
    ),
    policy_revision INTEGER NOT NULL CHECK (policy_revision >= 0),
    budget_units INTEGER NOT NULL CHECK (budget_units >= 1),
    outcome TEXT NOT NULL CHECK (outcome IN ('allow', 'deny')),
    reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 64),
    decided_at_unix_ms INTEGER NOT NULL CHECK (decided_at_unix_ms >= 0),
    FOREIGN KEY (capability_id) REFERENCES capabilities(id) ON DELETE NO ACTION
) STRICT;

CREATE INDEX capability_decisions_by_capability
    ON capability_decisions(capability_id, sequence);
CREATE INDEX capability_decisions_by_branch
    ON capability_decisions(session_id, branch_id, sequence);

CREATE TRIGGER capability_policy_revision_no_decrease
BEFORE UPDATE OF current_revision ON capability_policy_revisions
WHEN NEW.current_revision < OLD.current_revision
BEGIN
    SELECT RAISE(ABORT, 'capability policy revision cannot decrease');
END;

CREATE TRIGGER capability_ancestry_no_update
BEFORE UPDATE ON capability_ancestry
BEGIN
    SELECT RAISE(ABORT, 'capability ancestry is immutable');
END;

CREATE TRIGGER capability_ancestry_no_delete
BEFORE DELETE ON capability_ancestry
BEGIN
    SELECT RAISE(ABORT, 'capability ancestry is immutable');
END;

CREATE TRIGGER capability_authorizations_no_update
BEFORE UPDATE ON capability_authorizations
BEGIN
    SELECT RAISE(ABORT, 'capability authorization history is append-only');
END;

CREATE TRIGGER capability_authorizations_no_delete
BEFORE DELETE ON capability_authorizations
BEGIN
    SELECT RAISE(ABORT, 'capability authorization history is append-only');
END;

CREATE TRIGGER capability_decisions_no_update
BEFORE UPDATE ON capability_decisions
BEGIN
    SELECT RAISE(ABORT, 'capability decision history is append-only');
END;

CREATE TRIGGER capability_decisions_no_delete
BEFORE DELETE ON capability_decisions
BEGIN
    SELECT RAISE(ABORT, 'capability decision history is append-only');
END;

CREATE TRIGGER capabilities_immutable_scope
BEFORE UPDATE ON capabilities
WHEN NEW.id IS NOT OLD.id
  OR NEW.session_id IS NOT OLD.session_id
  OR NEW.branch_id IS NOT OLD.branch_id
  OR NEW.subject IS NOT OLD.subject
  OR NEW.action IS NOT OLD.action
  OR NEW.resource IS NOT OLD.resource
  OR NEW.constraints_json IS NOT OLD.constraints_json
  OR NEW.handle_hash IS NOT OLD.handle_hash
  OR NEW.delegated_from_id IS NOT OLD.delegated_from_id
  OR NEW.policy_revision IS NOT OLD.policy_revision
  OR NEW.issued_at_unix_ms IS NOT OLD.issued_at_unix_ms
  OR NEW.expires_at_unix_ms IS NOT OLD.expires_at_unix_ms
BEGIN
    SELECT RAISE(ABORT, 'capability scope is immutable');
END;

CREATE TRIGGER capabilities_no_counter_increase
BEFORE UPDATE ON capabilities
WHEN (OLD.remaining_uses IS NOT NULL AND NEW.remaining_uses IS NULL)
  OR (OLD.remaining_uses IS NOT NULL AND NEW.remaining_uses > OLD.remaining_uses)
  OR (OLD.remaining_budget_units IS NOT NULL AND NEW.remaining_budget_units IS NULL)
  OR (OLD.remaining_budget_units IS NOT NULL
      AND NEW.remaining_budget_units > OLD.remaining_budget_units)
BEGIN
    SELECT RAISE(ABORT, 'capability counters cannot increase');
END;

CREATE TRIGGER capabilities_no_reactivation
BEFORE UPDATE OF status ON capabilities
WHEN OLD.status <> 'active' AND NEW.status = 'active'
BEGIN
    SELECT RAISE(ABORT, 'inactive capability cannot be reactivated');
END;

CREATE TRIGGER capabilities_no_delete
BEFORE DELETE ON capabilities
BEGIN
    SELECT RAISE(ABORT, 'capability records cannot be deleted');
END;
