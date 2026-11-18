-- Backups of a folder here to a folder on a paired device, as snapshots.
-- On the device the folder holds `objects/<ab>/<blake3>` (each content once)
-- and `snapshots/<time>.json` (what the folder held at that time).
CREATE TABLE snapshot_sets (
    id                 TEXT PRIMARY KEY,
    name               TEXT NOT NULL,
    local_root_id      TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    local_path         TEXT NOT NULL,
    device_id          TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    remote_root_id     TEXT NOT NULL,
    remote_path        TEXT NOT NULL,
    remote_label       TEXT NOT NULL,
    every_hours        INTEGER NOT NULL DEFAULT 24,
    keep_last          INTEGER NOT NULL DEFAULT 7,
    keep_daily         INTEGER NOT NULL DEFAULT 14,
    keep_weekly        INTEGER NOT NULL DEFAULT 8,
    keep_monthly       INTEGER NOT NULL DEFAULT 12,
    enabled            INTEGER NOT NULL DEFAULT 1,
    created_by         TEXT NOT NULL,
    created_at         INTEGER NOT NULL,
    last_run_at        INTEGER,
    last_ok_at         INTEGER,
    last_result        TEXT,
    last_error         TEXT,
    last_check_at      INTEGER,
    last_check_result  TEXT
);

CREATE TABLE snapshots (
    id         TEXT PRIMARY KEY,
    set_id     TEXT NOT NULL REFERENCES snapshot_sets(id) ON DELETE CASCADE,
    taken_at   INTEGER NOT NULL,
    files      INTEGER NOT NULL,
    bytes      INTEGER NOT NULL,
    new_files  INTEGER NOT NULL,
    new_bytes  INTEGER NOT NULL,
    -- JSON list of {p, s, m, h}: path, size, modified, BLAKE3.
    manifest   TEXT NOT NULL
);
CREATE INDEX idx_snapshots_set ON snapshots (set_id, taken_at DESC);
