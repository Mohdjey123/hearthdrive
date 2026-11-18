-- Shared folders: a folder on this node given to people on this node or to
-- paired devices, on top of the storage locations they may see. Sharing
-- only ever adds access, and never more than the member's role allows.
CREATE TABLE shared_folders (
    id         TEXT PRIMARY KEY,
    root_id    TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    path       TEXT NOT NULL COLLATE NOCASE,    -- relative to the root, '' = all of it
    name       TEXT NOT NULL,                   -- what members see
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (root_id, path)
);

CREATE TABLE folder_members (
    folder_id TEXT NOT NULL REFERENCES shared_folders(id) ON DELETE CASCADE,
    member    TEXT NOT NULL,                    -- user:<id> | device:<id>
    access    TEXT NOT NULL,                    -- view | edit | add (add files only)
    added_at  INTEGER NOT NULL,
    PRIMARY KEY (folder_id, member)
);

CREATE INDEX folder_members_member ON folder_members (member);

-- A removed person or device keeps nothing: pairing again starts afresh.
CREATE TRIGGER folder_members_user_removed AFTER DELETE ON users
BEGIN
    DELETE FROM folder_members WHERE member = 'user:' || OLD.id;
END;

CREATE TRIGGER folder_members_device_removed AFTER DELETE ON devices
BEGIN
    DELETE FROM folder_members WHERE member = 'device:' || OLD.id;
END;

-- Comments on files. Kept by file id, which survives renames, moves and a
-- trip through the trash.
CREATE TABLE file_comments (
    id         TEXT PRIMARY KEY,
    file_id    TEXT NOT NULL,
    root_id    TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    author     TEXT NOT NULL,                   -- name, for display
    author_key TEXT NOT NULL,                   -- see AuthUser::owner_key
    body       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX file_comments_file ON file_comments (file_id, created_at);
