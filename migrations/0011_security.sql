-- Two-step sign-in, passkeys, locked accounts, blocked devices and the
-- sessions people can see and end.

-- Authenticator-app secret (protected with Windows DPAPI where available);
-- two-step sign-in is on once totp_enabled_at is set.
ALTER TABLE users ADD COLUMN totp_secret BLOB;
ALTER TABLE users ADD COLUMN totp_enabled_at INTEGER;
-- Time step of the last code accepted: each code works once.
ALTER TABLE users ADD COLUMN totp_last_step INTEGER;
-- A locked account cannot sign in and its sessions are ended.
ALTER TABLE users ADD COLUMN locked_at INTEGER;

-- One-time codes for when the authenticator app is not at hand.
CREATE TABLE recovery_codes (
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash  TEXT NOT NULL,                  -- BLAKE3 of the normalized code
    used_at    INTEGER,
    PRIMARY KEY (user_id, code_hash)
);

CREATE TABLE passkeys (
    id             TEXT PRIMARY KEY,
    user_id        TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    credential_id  TEXT NOT NULL UNIQUE,       -- base64url
    public_key     BLOB NOT NULL,              -- COSE key
    algorithm      INTEGER NOT NULL,
    sign_count     INTEGER NOT NULL DEFAULT 0,
    name           TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    last_used_at   INTEGER
);

CREATE INDEX idx_passkeys_user ON passkeys (user_id);

-- How each session was opened and when it was last used.
ALTER TABLE sessions ADD COLUMN method TEXT;
ALTER TABLE sessions ADD COLUMN last_used_at INTEGER;
CREATE INDEX idx_sessions_user ON sessions (user_id);

-- A blocked device's requests are refused and nothing is sent to it.
ALTER TABLE devices ADD COLUMN blocked_at INTEGER;
