CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN (
        'created', 'starting', 'running', 'suspended', 'checkpointing', 'restoring',
        'completed', 'failed'
    )),
    policy_revision INTEGER NOT NULL DEFAULT 0 CHECK (policy_revision >= 0),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at_unix_ms INTEGER NOT NULL CHECK (created_at_unix_ms >= 0),
    updated_at_unix_ms INTEGER NOT NULL CHECK (updated_at_unix_ms >= created_at_unix_ms)
) STRICT;

CREATE TABLE blobs (
    hash TEXT PRIMARY KEY CHECK (
        length(hash) = 64 AND hash NOT GLOB '*[^0-9a-f]*'
    ),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    media_type TEXT NOT NULL CHECK (length(media_type) BETWEEN 1 AND 255),
    created_at_unix_ms INTEGER NOT NULL CHECK (created_at_unix_ms >= 0)
) STRICT;

CREATE TABLE branches (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    parent_branch_id TEXT,
    fork_epoch_id TEXT,
    state TEXT NOT NULL CHECK (state IN (
        'created', 'running', 'suspended', 'completed', 'promoted', 'abandoned', 'failed'
    )),
    next_event_sequence INTEGER NOT NULL DEFAULT 0 CHECK (next_event_sequence >= 0),
    created_at_unix_ms INTEGER NOT NULL CHECK (created_at_unix_ms >= 0),
    updated_at_unix_ms INTEGER NOT NULL CHECK (updated_at_unix_ms >= created_at_unix_ms),
    UNIQUE (id, session_id),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE NO ACTION,
    FOREIGN KEY (parent_branch_id, session_id)
        REFERENCES branches(id, session_id) ON DELETE NO ACTION,
    FOREIGN KEY (fork_epoch_id, session_id)
        REFERENCES epochs(id, session_id) ON DELETE NO ACTION
) STRICT;

CREATE TABLE epochs (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    parent_epoch_id TEXT,
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    status TEXT NOT NULL CHECK (status IN ('creating', 'committed', 'failed')),
    backend TEXT,
    policy_revision INTEGER NOT NULL DEFAULT 0 CHECK (policy_revision >= 0),
    effect_frontier INTEGER NOT NULL DEFAULT 0 CHECK (effect_frontier >= 0),
    manifest_hash TEXT,
    created_at_unix_ms INTEGER NOT NULL CHECK (created_at_unix_ms >= 0),
    committed_at_unix_ms INTEGER CHECK (
        committed_at_unix_ms IS NULL OR committed_at_unix_ms >= created_at_unix_ms
    ),
    UNIQUE (id, session_id),
    UNIQUE (id, branch_id, session_id),
    UNIQUE (branch_id, sequence),
    FOREIGN KEY (branch_id, session_id)
        REFERENCES branches(id, session_id) ON DELETE NO ACTION,
    FOREIGN KEY (parent_epoch_id, branch_id, session_id)
        REFERENCES epochs(id, branch_id, session_id) ON DELETE NO ACTION,
    FOREIGN KEY (manifest_hash) REFERENCES blobs(hash) ON DELETE NO ACTION,
    CHECK (
        (status = 'committed' AND committed_at_unix_ms IS NOT NULL)
        OR (status != 'committed' AND committed_at_unix_ms IS NULL)
    )
) STRICT;

CREATE TABLE snapshot_components (
    epoch_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 128),
    status TEXT NOT NULL CHECK (status IN ('staged', 'committed', 'failed')),
    backend TEXT NOT NULL CHECK (length(backend) BETWEEN 1 AND 128),
    blob_hash TEXT,
    checksum_sha256 TEXT NOT NULL CHECK (
        length(checksum_sha256) = 64 AND checksum_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
    staged_at_unix_ms INTEGER NOT NULL CHECK (staged_at_unix_ms >= 0),
    committed_at_unix_ms INTEGER CHECK (
        committed_at_unix_ms IS NULL OR committed_at_unix_ms >= staged_at_unix_ms
    ),
    PRIMARY KEY (epoch_id, kind),
    FOREIGN KEY (epoch_id) REFERENCES epochs(id) ON DELETE NO ACTION,
    FOREIGN KEY (blob_hash) REFERENCES blobs(hash) ON DELETE NO ACTION,
    CHECK (
        (status = 'committed' AND blob_hash IS NOT NULL AND committed_at_unix_ms IS NOT NULL)
        OR status != 'committed'
    )
) STRICT;

CREATE TABLE events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    epoch_id TEXT,
    causal_parent_id TEXT,
    monotonic_ns INTEGER NOT NULL CHECK (monotonic_ns >= 0),
    occurred_at_unix_ms INTEGER NOT NULL CHECK (occurred_at_unix_ms >= 0),
    actor TEXT NOT NULL CHECK (actor IN ('agent', 'supervisor', 'tool', 'gateway', 'operator')),
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 128),
    input_hash TEXT,
    output_hash TEXT,
    status TEXT NOT NULL CHECK (status IN ('started', 'succeeded', 'failed', 'denied', 'unknown')),
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    UNIQUE (id, branch_id, session_id),
    UNIQUE (branch_id, sequence),
    FOREIGN KEY (branch_id, session_id)
        REFERENCES branches(id, session_id) ON DELETE NO ACTION,
    FOREIGN KEY (epoch_id, branch_id, session_id)
        REFERENCES epochs(id, branch_id, session_id) ON DELETE NO ACTION,
    FOREIGN KEY (causal_parent_id, branch_id, session_id)
        REFERENCES events(id, branch_id, session_id) ON DELETE NO ACTION,
    FOREIGN KEY (input_hash) REFERENCES blobs(hash) ON DELETE NO ACTION,
    FOREIGN KEY (output_hash) REFERENCES blobs(hash) ON DELETE NO ACTION
) STRICT;

CREATE TABLE capabilities (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    subject TEXT NOT NULL CHECK (length(subject) BETWEEN 1 AND 255),
    action TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 255),
    resource TEXT NOT NULL CHECK (length(resource) BETWEEN 1 AND 2048),
    constraints_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(constraints_json)),
    handle_hash TEXT NOT NULL UNIQUE CHECK (
        length(handle_hash) = 64 AND handle_hash NOT GLOB '*[^0-9a-f]*'
    ),
    delegated_from_id TEXT,
    remaining_uses INTEGER CHECK (remaining_uses IS NULL OR remaining_uses >= 0),
    policy_revision INTEGER NOT NULL CHECK (policy_revision >= 0),
    status TEXT NOT NULL CHECK (status IN ('active', 'consumed', 'expired', 'revoked')),
    issued_at_unix_ms INTEGER NOT NULL CHECK (issued_at_unix_ms >= 0),
    expires_at_unix_ms INTEGER CHECK (
        expires_at_unix_ms IS NULL OR expires_at_unix_ms >= issued_at_unix_ms
    ),
    updated_at_unix_ms INTEGER NOT NULL CHECK (updated_at_unix_ms >= issued_at_unix_ms),
    UNIQUE (id, branch_id, session_id),
    FOREIGN KEY (branch_id, session_id)
        REFERENCES branches(id, session_id) ON DELETE NO ACTION,
    FOREIGN KEY (delegated_from_id) REFERENCES capabilities(id) ON DELETE NO ACTION
) STRICT;

CREATE TABLE approvals (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 128),
    request_hash TEXT NOT NULL CHECK (
        length(request_hash) = 64 AND request_hash NOT GLOB '*[^0-9a-f]*'
    ),
    scope_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(scope_json)),
    status TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'denied', 'expired', 'cancelled')),
    operator_id TEXT,
    rationale TEXT,
    policy_revision INTEGER NOT NULL CHECK (policy_revision >= 0),
    requested_at_unix_ms INTEGER NOT NULL CHECK (requested_at_unix_ms >= 0),
    decided_at_unix_ms INTEGER CHECK (
        decided_at_unix_ms IS NULL OR decided_at_unix_ms >= requested_at_unix_ms
    ),
    FOREIGN KEY (branch_id, session_id)
        REFERENCES branches(id, session_id) ON DELETE NO ACTION
) STRICT;

CREATE TABLE effect_intents (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    branch_id TEXT NOT NULL,
    capability_id TEXT,
    operation_id TEXT NOT NULL UNIQUE CHECK (length(operation_id) BETWEEN 1 AND 255),
    replay_key TEXT NOT NULL CHECK (length(replay_key) BETWEEN 1 AND 255),
    action TEXT NOT NULL CHECK (length(action) BETWEEN 1 AND 255),
    resource TEXT NOT NULL CHECK (length(resource) BETWEEN 1 AND 2048),
    input_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'prepared', 'denied', 'dispatched', 'succeeded', 'failed', 'unknown'
    )),
    result_hash TEXT,
    error_json TEXT CHECK (error_json IS NULL OR json_valid(error_json)),
    policy_revision INTEGER NOT NULL CHECK (policy_revision >= 0),
    prepared_at_unix_ms INTEGER NOT NULL CHECK (prepared_at_unix_ms >= 0),
    dispatched_at_unix_ms INTEGER CHECK (
        dispatched_at_unix_ms IS NULL OR dispatched_at_unix_ms >= prepared_at_unix_ms
    ),
    resolved_at_unix_ms INTEGER CHECK (
        resolved_at_unix_ms IS NULL OR resolved_at_unix_ms >= prepared_at_unix_ms
    ),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    UNIQUE (branch_id, replay_key),
    FOREIGN KEY (branch_id, session_id)
        REFERENCES branches(id, session_id) ON DELETE NO ACTION,
    FOREIGN KEY (capability_id, branch_id, session_id)
        REFERENCES capabilities(id, branch_id, session_id) ON DELETE NO ACTION,
    FOREIGN KEY (input_hash) REFERENCES blobs(hash) ON DELETE NO ACTION,
    FOREIGN KEY (result_hash) REFERENCES blobs(hash) ON DELETE NO ACTION
) STRICT;

CREATE TABLE effect_attempts (
    id TEXT PRIMARY KEY,
    effect_id TEXT NOT NULL,
    attempt_no INTEGER NOT NULL CHECK (attempt_no >= 1),
    state TEXT NOT NULL CHECK (state IN ('started', 'succeeded', 'failed', 'unknown')),
    downstream_idempotency_key TEXT NOT NULL CHECK (
        length(downstream_idempotency_key) BETWEEN 1 AND 255
    ),
    downstream_reference TEXT,
    response_hash TEXT,
    error_json TEXT CHECK (error_json IS NULL OR json_valid(error_json)),
    started_at_unix_ms INTEGER NOT NULL CHECK (started_at_unix_ms >= 0),
    completed_at_unix_ms INTEGER CHECK (
        completed_at_unix_ms IS NULL OR completed_at_unix_ms >= started_at_unix_ms
    ),
    UNIQUE (effect_id, attempt_no),
    FOREIGN KEY (effect_id) REFERENCES effect_intents(id) ON DELETE NO ACTION,
    FOREIGN KEY (response_hash) REFERENCES blobs(hash) ON DELETE NO ACTION
) STRICT;

CREATE TABLE semantic_manifests (
    id TEXT PRIMARY KEY,
    epoch_id TEXT NOT NULL UNIQUE,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    digest TEXT NOT NULL CHECK (length(digest) BETWEEN 1 AND 255),
    blob_hash TEXT NOT NULL,
    created_at_unix_ms INTEGER NOT NULL CHECK (created_at_unix_ms >= 0),
    FOREIGN KEY (epoch_id) REFERENCES epochs(id) ON DELETE NO ACTION,
    FOREIGN KEY (blob_hash) REFERENCES blobs(hash) ON DELETE NO ACTION
) STRICT;

CREATE TABLE semantic_diffs (
    id TEXT PRIMARY KEY,
    left_epoch_id TEXT NOT NULL,
    right_epoch_id TEXT NOT NULL,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    digest TEXT NOT NULL CHECK (length(digest) BETWEEN 1 AND 255),
    blob_hash TEXT NOT NULL,
    summary_json TEXT NOT NULL CHECK (json_valid(summary_json)),
    created_at_unix_ms INTEGER NOT NULL CHECK (created_at_unix_ms >= 0),
    UNIQUE (left_epoch_id, right_epoch_id, schema_version),
    FOREIGN KEY (left_epoch_id) REFERENCES epochs(id) ON DELETE NO ACTION,
    FOREIGN KEY (right_epoch_id) REFERENCES epochs(id) ON DELETE NO ACTION,
    FOREIGN KEY (blob_hash) REFERENCES blobs(hash) ON DELETE NO ACTION,
    CHECK (left_epoch_id != right_epoch_id)
) STRICT;

CREATE TABLE benchmark_runs (
    id TEXT PRIMARY KEY,
    session_id TEXT,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'completed', 'failed')),
    backend TEXT NOT NULL CHECK (length(backend) BETWEEN 1 AND 128),
    code_revision TEXT NOT NULL CHECK (length(code_revision) BETWEEN 1 AND 255),
    hardware_json TEXT NOT NULL CHECK (json_valid(hardware_json)),
    kernel_json TEXT NOT NULL CHECK (json_valid(kernel_json)),
    config_json TEXT NOT NULL CHECK (json_valid(config_json)),
    seed INTEGER NOT NULL CHECK (seed >= 0),
    decision TEXT CHECK (decision IS NULL OR decision IN ('keep', 'narrow', 'kill')),
    rationale TEXT,
    result_blob_hash TEXT,
    started_at_unix_ms INTEGER NOT NULL CHECK (started_at_unix_ms >= 0),
    completed_at_unix_ms INTEGER CHECK (
        completed_at_unix_ms IS NULL OR completed_at_unix_ms >= started_at_unix_ms
    ),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE NO ACTION,
    FOREIGN KEY (result_blob_hash) REFERENCES blobs(hash) ON DELETE NO ACTION
) STRICT;

CREATE TABLE fault_injections (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    session_id TEXT,
    branch_id TEXT,
    point TEXT NOT NULL CHECK (length(point) BETWEEN 1 AND 255),
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    seed INTEGER NOT NULL CHECK (seed >= 0),
    config_json TEXT NOT NULL CHECK (json_valid(config_json)),
    status TEXT NOT NULL CHECK (status IN ('configured', 'triggered', 'skipped', 'failed')),
    created_at_unix_ms INTEGER NOT NULL CHECK (created_at_unix_ms >= 0),
    triggered_at_unix_ms INTEGER CHECK (
        triggered_at_unix_ms IS NULL OR triggered_at_unix_ms >= created_at_unix_ms
    ),
    UNIQUE (run_id, point, ordinal),
    FOREIGN KEY (run_id) REFERENCES benchmark_runs(id) ON DELETE NO ACTION,
    FOREIGN KEY (branch_id, session_id)
        REFERENCES branches(id, session_id) ON DELETE NO ACTION
) STRICT;

CREATE INDEX branches_by_session_state ON branches(session_id, state);
CREATE INDEX branches_by_parent ON branches(parent_branch_id);
CREATE INDEX epochs_by_branch_status_sequence ON epochs(branch_id, status, sequence);
CREATE INDEX events_by_session_time ON events(session_id, occurred_at_unix_ms, id);
CREATE INDEX events_by_branch_kind_sequence ON events(branch_id, kind, sequence);
CREATE INDEX events_by_epoch ON events(epoch_id);
CREATE INDEX events_by_causal_parent ON events(causal_parent_id);
CREATE INDEX capabilities_by_branch_status_expiry
    ON capabilities(branch_id, status, expires_at_unix_ms);
CREATE INDEX capabilities_by_session_status ON capabilities(session_id, status);
CREATE INDEX effects_by_branch_state ON effect_intents(branch_id, state);
CREATE INDEX effects_by_capability_state ON effect_intents(capability_id, state);
