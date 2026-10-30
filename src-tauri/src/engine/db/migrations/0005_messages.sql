-- C1b-M1 store, version 5 (A:418-429, plan C1b-M1 §3): the mail ledger.
-- `msg.send` writes a `messages` row; every state change appends a
-- `message_status` row and emits `msg.state` in the same transaction.
-- Differences from A:421-429: no `rate_buckets` (the pair index counts),
-- no channel/ttl/require_ack/chain/hop/grant columns yet (added by
-- ALTER TABLE when they are read), an extra `req_hash`, and `idem` is
-- unique per sender, not globally.

CREATE TABLE messages (
    id             TEXT PRIMARY KEY,               -- msg_<ulid>
    from_principal TEXT NOT NULL,                  -- 'agent:<id>' | 'local-unverified'
    from_agent     TEXT REFERENCES agents (id),    -- null for local-unverified
    to_agent       TEXT NOT NULL REFERENCES agents (id),
    to_addr        TEXT NOT NULL,                  -- <machine>/<id>
    origin_kind    TEXT NOT NULL CHECK (origin_kind IN ('agent', 'local-unverified')),
    kind           TEXT NOT NULL DEFAULT 'info' CHECK (kind IN ('request', 'info', 'result', 'system')),
    reply_to       TEXT,
    thread_id      TEXT,                           -- reply_to's thread, or the row's own id
    provenance     TEXT NOT NULL,                  -- the tag line injected above the body
    body           TEXT NOT NULL,
    body_hash      TEXT NOT NULL,                  -- sha256 hex of body
    req_hash       TEXT NOT NULL,                  -- sha256 hex of (to_agent, kind, reply_to, body)
    idem           TEXT,
    -- Current state, denormalized from the latest message_status row and
    -- written in the same transaction, so the delivery loop reads one row.
    state          TEXT NOT NULL CHECK (state IN ('queued', 'delivering', 'delivered', 'read',
                     'acked', 'failed', 'expired', 'rejected', 'uncertain', 'overdue')),
    state_at       INTEGER NOT NULL,
    attempts       INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL,
    cursor         TEXT                            -- cursor of the queued event, for idem replay
) STRICT;

CREATE UNIQUE INDEX messages_idem ON messages (from_principal, idem) WHERE idem IS NOT NULL;
CREATE INDEX messages_queue ON messages (to_agent, state, created_at);
CREATE INDEX messages_pair ON messages (from_principal, to_agent, created_at);

-- Append-only; a message's current state is its latest row (A:425).
CREATE TABLE message_status (
    seq    INTEGER PRIMARY KEY,
    msg_id TEXT NOT NULL REFERENCES messages (id),
    state  TEXT NOT NULL,
    at     INTEGER NOT NULL,
    detail TEXT
) STRICT;
CREATE INDEX message_status_msg ON message_status (msg_id, seq);
