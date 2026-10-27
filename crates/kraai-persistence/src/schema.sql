CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    data TEXT NOT NULL,
    tip_id TEXT,
    revision INTEGER NOT NULL DEFAULT 0,
    metadata_revision INTEGER NOT NULL DEFAULT 0,
    lease_active INTEGER NOT NULL DEFAULT 0,
    lease_expires_at INTEGER NOT NULL DEFAULT 0,
    lease_token INTEGER NOT NULL DEFAULT 0,
    turn_timer TEXT
);
CREATE TABLE IF NOT EXISTS records (
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    session_id TEXT,
    data TEXT NOT NULL,
    PRIMARY KEY (kind, id)
);
CREATE INDEX IF NOT EXISTS records_session ON records(session_id, kind);
CREATE INDEX IF NOT EXISTS records_message_parent ON records(json_extract(data, '$.parent_id'))
    WHERE kind = 'message' AND json_valid(data);
CREATE INDEX IF NOT EXISTS sessions_tip ON sessions(tip_id);
CREATE TABLE IF NOT EXISTS execution_sources (
    execution_id TEXT PRIMARY KEY,
    source BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS execution_output (
    sequence INTEGER PRIMARY KEY,
    execution_id TEXT NOT NULL,
    stream TEXT NOT NULL,
    bytes BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS execution_output_id ON execution_output(execution_id, sequence);
CREATE TABLE IF NOT EXISTS stream_deltas (
    message_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (message_id, sequence)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS queued_messages (
    session_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (session_id, sequence)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS model_lists (
    provider_id TEXT PRIMARY KEY,
    source TEXT NOT NULL DEFAULT '',
    fetched_at INTEGER NOT NULL DEFAULT 0,
    data TEXT,
    claimed_until INTEGER NOT NULL DEFAULT 0
);
