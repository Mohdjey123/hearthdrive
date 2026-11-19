-- Photos, media, documents, tags, rules, API tokens and Wake-on-LAN.

-- What NEXUS read from inside a file: photo details (EXIF), music tags,
-- film and episode names, a perceptual hash for similar photos.
-- Derived data: it goes with the file and is read again when the file changes.
CREATE TABLE media_info (
    file_id      TEXT PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    root_id      TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    modified_at  INTEGER,                 -- the file's mtime when it was read
    kind         TEXT NOT NULL,           -- photo | video | music | other
    taken_at     INTEGER,                 -- photo or video date (ms, local time as UTC)
    width        INTEGER,
    height       INTEGER,
    camera       TEXT,
    lens         TEXT,
    latitude     REAL,
    longitude    REAL,
    duration_ms  INTEGER,
    phash        TEXT,                    -- 64-bit difference hash, hex
    title        TEXT,
    artist       TEXT,
    album        TEXT,
    album_artist TEXT,
    track        INTEGER,
    disc         INTEGER,
    year         INTEGER,
    genre        TEXT,
    show_name    TEXT,                    -- series name for episodes
    season       INTEGER,
    episode      INTEGER,
    media_type   TEXT,                    -- film | episode | music | home_video
    faces        INTEGER,                 -- faces found in a photo, when known
    labels       TEXT,                    -- words describing a photo, space separated
    read_at      INTEGER NOT NULL,
    error        TEXT
);
CREATE INDEX idx_media_taken ON media_info (kind, taken_at DESC);
CREATE INDEX idx_media_type ON media_info (media_type, show_name, season, episode);
CREATE INDEX idx_media_album ON media_info (album_artist, album, disc, track);
CREATE INDEX idx_media_place ON media_info (latitude, longitude) WHERE latitude IS NOT NULL;

CREATE TABLE albums (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    created_by TEXT NOT NULL,             -- user:<id> or the local owner
    created_at INTEGER NOT NULL,
    cover_id   TEXT
);
CREATE TABLE album_items (
    album_id TEXT NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    file_id  TEXT NOT NULL,
    root_id  TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    added_by TEXT NOT NULL,
    added_at INTEGER NOT NULL,
    PRIMARY KEY (album_id, file_id)
);
CREATE INDEX idx_album_items_file ON album_items (file_id);
-- People an album is shared with; they see its photos even outside their storage.
CREATE TABLE album_members (
    album_id TEXT NOT NULL REFERENCES albums(id) ON DELETE CASCADE,
    member   TEXT NOT NULL,               -- user:<id> | device:<id>
    can_add  INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (album_id, member)
);

-- Where someone stopped watching or listening.
CREATE TABLE playback (
    viewer      TEXT NOT NULL,            -- user:<id> | device:<id> | local
    file_id     TEXT NOT NULL,
    position_ms INTEGER NOT NULL,
    duration_ms INTEGER,
    device_name TEXT,
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (viewer, file_id)
);
CREATE INDEX idx_playback_recent ON playback (viewer, updated_at DESC);

-- Text inside documents (PDF, Office, plain text, and photos of text).
CREATE VIRTUAL TABLE doc_text USING fts5 (file_id UNINDEXED, body, tokenize = 'unicode61 remove_diacritics 2');
CREATE TABLE doc_state (
    file_id     TEXT PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    root_id     TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    modified_at INTEGER,
    method      TEXT,                     -- text | pdf | office | ocr | none
    chars       INTEGER NOT NULL DEFAULT 0,
    read_at     INTEGER NOT NULL,
    error       TEXT
);
CREATE TRIGGER doc_state_gone AFTER DELETE ON doc_state BEGIN
    DELETE FROM doc_text WHERE file_id = OLD.file_id;
END;

-- Tags people give files. They stay through renames and the trash.
CREATE TABLE file_tags (
    file_id  TEXT NOT NULL,
    root_id  TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    tag      TEXT NOT NULL COLLATE NOCASE,
    added_by TEXT NOT NULL,
    auto     INTEGER NOT NULL DEFAULT 0,  -- set by a rule
    added_at INTEGER NOT NULL,
    PRIMARY KEY (file_id, tag)
);
CREATE INDEX idx_file_tags_tag ON file_tags (tag);

-- Automation: when something happens (or on a schedule), do something to
-- the files that match.
CREATE TABLE rules (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    trigger     TEXT NOT NULL,            -- added | daily | hourly
    root_id     TEXT NOT NULL REFERENCES storage_roots(id) ON DELETE CASCADE,
    path        TEXT NOT NULL DEFAULT '' COLLATE NOCASE,
    conditions  TEXT NOT NULL,            -- JSON
    action      TEXT NOT NULL,            -- JSON
    created_by  TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    last_run    INTEGER,
    last_result TEXT,
    files_done  INTEGER NOT NULL DEFAULT 0
);

-- Files a rule has already dealt with, so it never acts on one twice.
CREATE TABLE rule_done (
    rule_id  TEXT NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
    file_id  TEXT NOT NULL,
    done_at  INTEGER NOT NULL,
    PRIMARY KEY (rule_id, file_id)
) WITHOUT ROWID;

-- Tokens for programs: the public API, WebDAV and the S3-compatible API.
CREATE TABLE api_tokens (
    id         TEXT PRIMARY KEY,          -- also the S3 access key id
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    secret     TEXT NOT NULL,             -- sealed with the node key (S3 signs with it)
    read_only  INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    last_used  INTEGER,
    expires_at INTEGER
);

-- Network cards of paired devices, to wake them up.
ALTER TABLE devices ADD COLUMN mac_addresses TEXT;
ALTER TABLE devices ADD COLUMN broadcast TEXT;

-- Finding other copies of a file by content (downloads from several devices).
CREATE INDEX IF NOT EXISTS idx_files_content_hash ON files(content_hash) WHERE content_hash IS NOT NULL;
