CREATE TABLE annotations (
    target_kind TEXT NOT NULL CHECK (target_kind = 'session'),
    target_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    name TEXT NOT NULL,
    value_type TEXT NOT NULL CHECK (value_type IN ('number','string','bool','duration','money')),
    value_json TEXT NOT NULL CHECK (json_valid(value_json)),
    label TEXT,
    unit TEXT,
    format TEXT,
    sensitivity TEXT NOT NULL CHECK (sensitivity IN ('metadata','conversation')),
    writer_principal TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    expires_at INTEGER,
    PRIMARY KEY (target_kind, target_id, namespace, name)
) STRICT;
CREATE INDEX annotations_expiry ON annotations(expires_at) WHERE expires_at IS NOT NULL;
-- Sequence tombstones survive a clear/expiry, and engine restarts.
CREATE TABLE annotation_sources (
    target_kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    last_seq INTEGER,
    gone_at INTEGER,
    PRIMARY KEY (target_kind, target_id, namespace)
) STRICT;
CREATE TABLE annotation_rates (
    principal TEXT PRIMARY KEY,
    tokens REAL NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
ALTER TABLE audit ADD COLUMN annotation_keys TEXT;
