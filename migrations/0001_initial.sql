-- NEXUS Cloud initial schema.
-- Each node owns its own database. Nodes never share or replicate this file;
-- they exchange structured metadata events instead.
-- Timestamps are Unix epoch milliseconds.

CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE devices (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    device_type     TEXT NOT NULL,              -- windows | android | web
    public_key      TEXT,                       -- set during pairing (Phase 2)
    network_address TEXT,
    is_local        INTEGER NOT NULL DEFAULT 0,
    last_seen       INTEGER,
    status          TEXT NOT NULL,              -- online | offline | revoked
    created_at      INTEGER NOT NULL
);

CREATE TABLE storage_roots (
    id            TEXT PRIMARY KEY,
    device_id     TEXT NOT NULL REFERENCES devices(id),
    name          TEXT NOT NULL,
    absolute_path TEXT NOT NULL COLLATE NOCASE,
    root_type     TEXT NOT NULL,                -- managed | existing | external
    read_only     INTEGER NOT NULL DEFAULT 0,
    enabled       INTEGER NOT NULL DEFAULT 1,
    created_at    INTEGER NOT NULL,
    UNIQUE (device_id, absolute_path)
);

CREATE TABLE root_scan_state (
    root_id      TEXT PRIMARY KEY REFERENCES storage_roots(id) ON DELETE CASCADE,
    status       TEXT NOT NULL,                 -- idle | scanning | error | offline
    files_seen   INTEGER NOT NULL DEFAULT 0,
    bytes_seen   INTEGER NOT NULL DEFAULT 0,
    started_at   INTEGER,
    completed_at INTEGER,
    error        TEXT
);

-- Paths are stored relative to their storage root, '/'-separated, without a
-- leading slash. Windows filesystems are case-insensitive, so paths are too.
CREATE TABLE files (
    id            TEXT PRIMARY KEY,
    root_id       TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL COLLATE NOCASE,
    parent_path   TEXT NOT NULL COLLATE NOCASE, -- '' for top-level entries
    filename      TEXT NOT NULL,
    is_dir        INTEGER NOT NULL DEFAULT 0,
    extension     TEXT,
    mime_type     TEXT,
    size          INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER,
    modified_at   INTEGER,
    content_hash  TEXT,                         -- BLAKE3, computed in the background
    hashed_at     INTEGER,
    indexed_at    INTEGER NOT NULL,
    availability  TEXT NOT NULL DEFAULT 'online',
    UNIQUE (root_id, relative_path)
);

CREATE INDEX idx_files_parent   ON files (root_id, parent_path);
CREATE INDEX idx_files_filename ON files (filename COLLATE NOCASE);
CREATE INDEX idx_files_modified ON files (modified_at DESC) WHERE is_dir = 0;
CREATE INDEX idx_files_unhashed ON files (root_id) WHERE hashed_at IS NULL AND is_dir = 0;

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT,                         -- Argon2id PHC string
    role          TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);

CREATE TABLE sessions (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash   TEXT NOT NULL UNIQUE,          -- BLAKE3 of the bearer token
    client       TEXT,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
);

CREATE INDEX idx_sessions_expires ON sessions (expires_at);

CREATE TABLE deleted_files (
    id            TEXT PRIMARY KEY,             -- the original file id
    root_id       TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    original_path TEXT NOT NULL,
    filename      TEXT NOT NULL,
    is_dir        INTEGER NOT NULL DEFAULT 0,
    size          INTEGER NOT NULL DEFAULT 0,
    trash_name    TEXT NOT NULL,
    deleted_at    INTEGER NOT NULL,
    deleted_by    TEXT
);

CREATE TABLE activity_log (
    id        TEXT PRIMARY KEY,
    device_id TEXT,
    user_id   TEXT,
    action    TEXT NOT NULL,
    resource  TEXT,
    timestamp INTEGER NOT NULL
);

CREATE INDEX idx_activity_timestamp ON activity_log (timestamp DESC);
