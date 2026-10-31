-- Scoped gateway credentials; no plaintext token or pairing code is stored.
CREATE TABLE gateway_tokens (
    client_id TEXT PRIMARY KEY,
    sha256 BLOB NOT NULL UNIQUE CHECK (length(sha256) = 32),
    label TEXT NOT NULL,
    origin TEXT,
    aud TEXT NOT NULL,
    scopes TEXT NOT NULL CHECK (json_valid(scopes) AND json_type(scopes) = 'array'),
    ceiling TEXT NOT NULL CHECK (ceiling IN ('public', 'metadata', 'conversation')),
    created_at INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL CHECK (last_used_at >= created_at),
    expires_at INTEGER NOT NULL CHECK (expires_at > created_at AND expires_at <= created_at + 15552000000)
);
CREATE INDEX gateway_tokens_expiry ON gateway_tokens (expires_at);
