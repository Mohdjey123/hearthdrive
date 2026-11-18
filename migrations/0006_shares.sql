-- Share links: a file or folder uploaded to a link service (Send, end-to-end
-- encrypted, or temp.sh) so anyone with the link can download it.
CREATE TABLE shares (
    id           TEXT PRIMARY KEY,
    file_id      TEXT NOT NULL,                 -- what was shared (it may since be gone)
    root_id      TEXT NOT NULL,
    name         TEXT NOT NULL,                 -- what recipients download
    is_dir       INTEGER NOT NULL DEFAULT 0,    -- a folder, sent as a zip file
    size         INTEGER NOT NULL DEFAULT 0,
    provider     TEXT NOT NULL,                 -- send | tempsh
    service      TEXT NOT NULL,                 -- the service's address
    url          TEXT,                          -- the full link, key included
    remote_id    TEXT,
    owner_token  TEXT,                          -- lets this node delete it there
    expires_at   INTEGER NOT NULL,
    downloads    INTEGER,                       -- download limit (Send)
    downloaded   INTEGER NOT NULL DEFAULT 0,
    has_password INTEGER NOT NULL DEFAULT 0,
    status       TEXT NOT NULL,                 -- uploading | active | expired | failed | deleted
    error        TEXT,
    job_id       TEXT,
    owner_key    TEXT NOT NULL,                 -- who made it (see AuthUser::owner_key)
    created_by   TEXT NOT NULL,                 -- their name, for display
    created_at   INTEGER NOT NULL
);

CREATE INDEX shares_created ON shares (created_at);
