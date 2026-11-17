-- Resumable uploads: the bytes live in a staging file named after the id;
-- this row remembers where the finished file goes.
CREATE TABLE uploads (
    id            TEXT PRIMARY KEY,
    root_id       TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    parent_path   TEXT NOT NULL,
    filename      TEXT NOT NULL,
    size          INTEGER NOT NULL,
    on_conflict   TEXT NOT NULL DEFAULT 'rename', -- rename | fail | replace
    modified_at   INTEGER,                        -- applied to the finished file
    make_parents  INTEGER NOT NULL DEFAULT 0,
    owner         TEXT NOT NULL,                  -- who started it (username as authenticated)
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

-- Earlier contents of a file, kept when NEXUS replaces it (sync, copy with
-- replace, restoring a version). The bytes live in .nexus/versions/<id>.
CREATE TABLE file_versions (
    id            TEXT PRIMARY KEY,
    file_id       TEXT NOT NULL,
    root_id       TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    size          INTEGER NOT NULL,
    modified_at   INTEGER,
    content_hash  TEXT,
    created_at    INTEGER NOT NULL,
    created_by    TEXT
);
CREATE INDEX idx_versions_file ON file_versions (file_id, created_at DESC);

-- A folder here kept in step with a folder on a paired device.
CREATE TABLE sync_pairs (
    id             TEXT PRIMARY KEY,
    local_root_id  TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    local_path     TEXT NOT NULL,
    device_id      TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    remote_root_id TEXT NOT NULL,
    remote_path    TEXT NOT NULL,
    remote_label   TEXT NOT NULL,                 -- "Storage / folder" on the device, for display
    mode           TEXT NOT NULL,                 -- two_way | upload | download
    enabled        INTEGER NOT NULL DEFAULT 1,
    created_by     TEXT NOT NULL,                 -- username the device is asked to act for
    created_role   TEXT NOT NULL,
    last_run_at    INTEGER,
    last_ok_at     INTEGER,
    last_result    TEXT,
    last_error     TEXT,
    created_at     INTEGER NOT NULL
);

-- What each side looked like after the last successful sync of a path, so a
-- change on either side can be told apart from a change on the other.
CREATE TABLE sync_state (
    pair_id     TEXT NOT NULL REFERENCES sync_pairs(id) ON DELETE CASCADE,
    rel_path    TEXT NOT NULL,
    local_fp    TEXT,
    remote_fp   TEXT,
    PRIMARY KEY (pair_id, rel_path)
);
