-- Extra copies: a sync pair in mode 'copy' keeps a one-way copy of a folder
-- on a paired device. Files removed here stay on the copy for keep_days
-- before they go to the device's trash.
ALTER TABLE sync_pairs ADD COLUMN keep_days INTEGER NOT NULL DEFAULT 30;
ALTER TABLE sync_pairs ADD COLUMN last_check_at INTEGER;
ALTER TABLE sync_pairs ADD COLUMN last_check_result TEXT;

-- Files removed here that a copy still holds, and since when.
CREATE TABLE copy_held (
    pair_id  TEXT NOT NULL REFERENCES sync_pairs(id) ON DELETE CASCADE,
    rel_path TEXT NOT NULL,              -- key, lower-cased as in sync_state
    path     TEXT NOT NULL,              -- as the device spells it
    is_dir   INTEGER NOT NULL DEFAULT 0,
    size     INTEGER NOT NULL DEFAULT 0,
    gone_at  INTEGER NOT NULL,
    PRIMARY KEY (pair_id, rel_path)
);

-- Drive health: the last full read of each storage location's files.
CREATE TABLE root_checks (
    root_id    TEXT PRIMARY KEY REFERENCES storage_roots(id) ON DELETE CASCADE,
    checked_at INTEGER NOT NULL,
    files      INTEGER NOT NULL,
    bytes      INTEGER NOT NULL,
    damaged    INTEGER NOT NULL,
    result     TEXT NOT NULL
);

-- Files whose content no longer matches the hash taken when they were
-- indexed, although their size and date did not change.
CREATE TABLE damaged_files (
    file_id       TEXT PRIMARY KEY,
    root_id       TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    size          INTEGER NOT NULL,
    expected_hash TEXT NOT NULL,
    found_hash    TEXT,                  -- NULL: the file could not be read
    found_at      INTEGER NOT NULL
);
CREATE INDEX idx_damaged_root ON damaged_files (root_id);

-- Whether each storage location was there when last looked at, and since when.
CREATE TABLE root_presence (
    root_id TEXT PRIMARY KEY REFERENCES storage_roots(id) ON DELETE CASCADE,
    online  INTEGER NOT NULL,
    since   INTEGER NOT NULL
);
