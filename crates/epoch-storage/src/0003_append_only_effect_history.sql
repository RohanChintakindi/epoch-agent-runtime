CREATE TABLE effect_transition_history (
    effect_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    state TEXT NOT NULL CHECK (state IN (
        'requested', 'prepared', 'dispatched', 'committed', 'failed', 'unknown'
    )),
    occurred_at_unix_ms INTEGER NOT NULL CHECK (occurred_at_unix_ms >= 0),
    detail_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(detail_json)),
    PRIMARY KEY (effect_id, sequence),
    FOREIGN KEY (effect_id) REFERENCES effect_intents(id) ON DELETE NO ACTION
) STRICT;

CREATE TABLE effect_attempt_history (
    effect_id TEXT NOT NULL,
    attempt_no INTEGER NOT NULL CHECK (attempt_no >= 1),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    state TEXT NOT NULL CHECK (state IN ('started', 'committed', 'failed', 'unknown')),
    occurred_at_unix_ms INTEGER NOT NULL CHECK (occurred_at_unix_ms >= 0),
    detail_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(detail_json)),
    PRIMARY KEY (effect_id, attempt_no, sequence),
    FOREIGN KEY (effect_id, attempt_no)
        REFERENCES effect_attempts(effect_id, attempt_no) ON DELETE NO ACTION
) STRICT;

CREATE INDEX effect_transition_history_by_state
    ON effect_transition_history(state, occurred_at_unix_ms);
CREATE INDEX effect_attempt_history_by_state
    ON effect_attempt_history(state, occurred_at_unix_ms);

CREATE TRIGGER effect_transition_history_no_update
BEFORE UPDATE ON effect_transition_history
BEGIN
    SELECT RAISE(ABORT, 'effect transition history is append-only');
END;

CREATE TRIGGER effect_transition_history_no_delete
BEFORE DELETE ON effect_transition_history
BEGIN
    SELECT RAISE(ABORT, 'effect transition history is append-only');
END;

CREATE TRIGGER effect_attempt_history_no_update
BEFORE UPDATE ON effect_attempt_history
BEGIN
    SELECT RAISE(ABORT, 'effect attempt history is append-only');
END;

CREATE TRIGGER effect_attempt_history_no_delete
BEFORE DELETE ON effect_attempt_history
BEGIN
    SELECT RAISE(ABORT, 'effect attempt history is append-only');
END;
