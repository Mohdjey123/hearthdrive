-- Groups of people on this node ("Family", "Office"), so a folder can be
-- shared with all of them at once. Someone added to a group gets its
-- folders straight away; someone removed loses them.
CREATE TABLE people_groups (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    created_by TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE group_members (
    group_id TEXT NOT NULL REFERENCES people_groups(id) ON DELETE CASCADE,
    user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    added_at INTEGER NOT NULL,
    PRIMARY KEY (group_id, user_id)
) WITHOUT ROWID;

CREATE INDEX group_members_user ON group_members (user_id);

-- A removed group is taken off every folder shared with it.
CREATE TRIGGER folder_members_group_removed AFTER DELETE ON people_groups
BEGIN
    DELETE FROM folder_members WHERE member = 'group:' || OLD.id;
END;

-- Upload links: a page anyone with the link can add files to a folder from
-- (a file request), served on the address for programs. Files are added as
-- the person who made the link, never replace anything, and stop at the
-- link's size limit and expiry.
CREATE TABLE upload_links (
    id             TEXT PRIMARY KEY,
    secret         TEXT NOT NULL,              -- sealed with this node's key
    root_id        TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    path           TEXT NOT NULL,
    title          TEXT NOT NULL,
    message        TEXT NOT NULL DEFAULT '',
    user_id        TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_by     TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER,
    max_bytes      INTEGER NOT NULL,
    received_bytes INTEGER NOT NULL DEFAULT 0,
    received_files INTEGER NOT NULL DEFAULT 0,
    last_used_at   INTEGER
);
