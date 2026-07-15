ALTER TABLE branches
    ADD COLUMN name TEXT CHECK (
        name IS NULL OR (
            length(name) BETWEEN 1 AND 64
            AND substr(name, 1, 1) GLOB '[a-z0-9]'
            AND name NOT GLOB '*[^a-z0-9._-]*'
        )
    );

ALTER TABLE branches
    ADD COLUMN fork_point_sequence INTEGER CHECK (
        fork_point_sequence IS NULL OR fork_point_sequence >= 0
    );

ALTER TABLE branches
    ADD COLUMN fork_component_hash TEXT REFERENCES blobs(hash) ON DELETE NO ACTION CHECK (
        fork_component_hash IS NULL OR (
            length(fork_component_hash) = 64
            AND fork_component_hash NOT GLOB '*[^0-9a-f]*'
        )
    );

CREATE UNIQUE INDEX branches_unique_fork_name
    ON branches(session_id, name)
    WHERE name IS NOT NULL;

CREATE TRIGGER branches_validate_fork_lineage_insert
BEFORE INSERT ON branches
WHEN
    (NEW.parent_branch_id IS NULL) != (NEW.fork_epoch_id IS NULL)
    OR (NEW.parent_branch_id IS NULL) != (NEW.name IS NULL)
    OR (NEW.parent_branch_id IS NULL) != (NEW.fork_point_sequence IS NULL)
    OR (NEW.parent_branch_id IS NULL) != (NEW.fork_component_hash IS NULL)
BEGIN
    SELECT RAISE(ABORT, 'fork lineage must be complete');
END;

CREATE TRIGGER branches_validate_fork_source_insert
BEFORE INSERT ON branches
WHEN NEW.fork_epoch_id IS NOT NULL
BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1
        FROM epochs e
        JOIN snapshot_components c ON c.epoch_id = e.id
        WHERE e.id = NEW.fork_epoch_id
          AND e.session_id = NEW.session_id
          AND e.branch_id = NEW.parent_branch_id
          AND e.status = 'committed'
          AND c.kind = 'application_context'
          AND c.status = 'committed'
          AND c.blob_hash = NEW.fork_component_hash
          AND json_extract(c.metadata_json, '$.boundary_sequence') = NEW.fork_point_sequence
    ) THEN RAISE(ABORT, 'fork source is not an exact committed application checkpoint') END;
END;

CREATE TRIGGER branches_reject_lineage_update
BEFORE UPDATE OF
    session_id, parent_branch_id, fork_epoch_id, name, fork_point_sequence, fork_component_hash
ON branches
WHEN
    OLD.session_id IS NOT NEW.session_id
    OR OLD.parent_branch_id IS NOT NEW.parent_branch_id
    OR OLD.fork_epoch_id IS NOT NEW.fork_epoch_id
    OR OLD.name IS NOT NEW.name
    OR OLD.fork_point_sequence IS NOT NEW.fork_point_sequence
    OR OLD.fork_component_hash IS NOT NEW.fork_component_hash
BEGIN
    SELECT RAISE(ABORT, 'branch lineage is immutable');
END;

CREATE TRIGGER branches_reject_fork_delete
BEFORE DELETE ON branches
WHEN OLD.fork_epoch_id IS NOT NULL
BEGIN
    SELECT RAISE(ABORT, 'fork lineage is immutable');
END;

CREATE TRIGGER effect_intents_reject_delete
BEFORE DELETE ON effect_intents
BEGIN
    SELECT RAISE(ABORT, 'effect history is non-rollbackable');
END;

CREATE TRIGGER effect_attempts_reject_delete
BEFORE DELETE ON effect_attempts
BEGIN
    SELECT RAISE(ABORT, 'effect history is non-rollbackable');
END;
