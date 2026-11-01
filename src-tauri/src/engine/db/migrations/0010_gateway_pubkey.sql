-- D15: gateway clients authenticate with their own Ed25519 key. Core stores
-- only the public key, so the db holds no replayable secret. The bearer-token
-- table had no writer: no route ever issued a token, so it holds no rows.
CREATE TABLE gateway_clients (
    client_id TEXT PRIMARY KEY CHECK (length(client_id) = 30 AND client_id GLOB 'cli_*'),
    public_key BLOB NOT NULL UNIQUE CHECK (length(public_key) = 32),
    label TEXT NOT NULL,
    origin TEXT,
    aud TEXT NOT NULL,
    scopes TEXT NOT NULL CHECK (json_valid(scopes) AND json_type(scopes) = 'array'),
    ceiling TEXT NOT NULL CHECK (ceiling IN ('public', 'metadata', 'conversation')),
    created_at INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL CHECK (last_used_at >= created_at),
    expires_at INTEGER NOT NULL CHECK (expires_at > created_at AND expires_at <= created_at + 15552000000)
);
CREATE INDEX gateway_clients_expiry ON gateway_clients (expires_at);
DROP INDEX gateway_tokens_expiry;
DROP TABLE gateway_tokens;
