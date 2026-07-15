ALTER TABLE events
    ADD COLUMN payload_blob_hash TEXT REFERENCES blobs(hash) ON DELETE NO ACTION;

CREATE INDEX events_by_payload_blob ON events(payload_blob_hash);

CREATE TRIGGER events_reject_update
BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_reject_delete
BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;
