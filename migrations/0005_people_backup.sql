-- Phase 6: which storage locations a person may see. NULL means every
-- location; otherwise a JSON array of storage root ids, which may include
-- locations on paired devices. Owners and administrators are never limited.
ALTER TABLE users ADD COLUMN allowed_roots TEXT;

-- Phone backup: photos and videos on this phone copied to a folder on a
-- paired computer. One per phone.
CREATE TABLE backups (
    id           TEXT PRIMARY KEY,
    device_id    TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    root_id      TEXT NOT NULL,                 -- storage location on that device
    path         TEXT NOT NULL,                 -- folder in it
    label        TEXT NOT NULL,                 -- "Storage / folder", for display
    sources      TEXT NOT NULL,                 -- JSON array of phone folders (DCIM, Pictures, Movies)
    enabled      INTEGER NOT NULL DEFAULT 1,
    created_by   TEXT NOT NULL,                 -- username the device is asked to act for
    last_run_at  INTEGER,
    last_ok_at   INTEGER,
    last_result  TEXT,
    last_error   TEXT,
    created_at   INTEGER NOT NULL
);

-- Files already backed up, with their size and modification time then, so
-- each run sends only what is new or changed, and a file deleted on the
-- computer is not sent again.
CREATE TABLE backup_files (
    backup_id   TEXT NOT NULL REFERENCES backups(id) ON DELETE CASCADE,
    rel_path    TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    PRIMARY KEY (backup_id, rel_path)
);
