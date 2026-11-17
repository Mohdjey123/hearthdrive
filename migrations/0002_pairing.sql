-- Phase 2: device identity, pairing and peer presence.

ALTER TABLE devices ADD COLUMN role TEXT NOT NULL DEFAULT 'editor';  -- what a paired device may do here
ALTER TABLE devices ADD COLUMN addresses TEXT NOT NULL DEFAULT '[]'; -- JSON array of host:port on the private network
ALTER TABLE devices ADD COLUMN latency_ms INTEGER;
ALTER TABLE devices ADD COLUMN paired_at INTEGER;
ALTER TABLE devices ADD COLUMN version TEXT;

-- An invitation shown as a QR code / link. Only the hash of its code is kept.
CREATE TABLE pairing_sessions (
    id         TEXT PRIMARY KEY,
    code_hash  TEXT NOT NULL UNIQUE,
    created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    closed_at  INTEGER                             -- used, cancelled or superseded
);

-- A device asking to join through an invitation; waits for approval here.
CREATE TABLE pairing_requests (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES pairing_sessions(id) ON DELETE CASCADE,
    device_id   TEXT NOT NULL,
    name        TEXT NOT NULL,
    device_type TEXT NOT NULL,
    public_key  TEXT NOT NULL,
    addresses   TEXT NOT NULL DEFAULT '[]',
    version     TEXT,
    remote_ip   TEXT,
    status      TEXT NOT NULL,                     -- pending | approved | rejected | expired
    created_at  INTEGER NOT NULL,
    decided_at  INTEGER,
    decided_by  TEXT REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX idx_pairing_requests_status ON pairing_requests (status, created_at);
