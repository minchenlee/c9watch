-- C1b-M2: runner disables FK outside the transaction and checks before commit.
CREATE TABLE messages_new (
    id             TEXT PRIMARY KEY,               -- msg_<ulid>
    from_principal TEXT NOT NULL,                  -- 'agent:<id>' | 'local-unverified'
    from_agent     TEXT REFERENCES agents (id),    -- null for local-unverified
    to_agent       TEXT NOT NULL REFERENCES agents (id),
    to_addr        TEXT NOT NULL,                  -- <machine>/<id>
    origin_kind    TEXT NOT NULL CHECK (origin_kind IN ('agent', 'local-unverified', 'engine')),
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
                     'acked', 'failed', 'expired', 'rejected', 'uncertain', 'overdue', 'cancelled')),
    state_at       INTEGER NOT NULL,
    attempts       INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL,
    cursor         TEXT                            -- cursor of the queued event, for idem replay
    ,via TEXT CHECK (via IN ('pty', 'socket'))
    ,wire_id TEXT NOT NULL
    ,not_before INTEGER
    ,hop INTEGER NOT NULL DEFAULT 0
    ,notice_id TEXT REFERENCES messages (id)
) STRICT;

INSERT INTO messages_new (id, from_principal, from_agent, to_agent, to_addr, origin_kind, kind, reply_to, thread_id, provenance, body, body_hash, req_hash, idem, state, state_at, attempts, created_at, cursor, wire_id) SELECT id, from_principal, from_agent, to_agent, to_addr, origin_kind, kind, reply_to, thread_id, provenance, body, body_hash, req_hash, idem, state, state_at, attempts, created_at, cursor, lower(hex(randomblob(4)))||'-'||lower(hex(randomblob(2)))||'-4'||substr(lower(hex(randomblob(2))),2)||'-'||substr('89ab',1+(random() & 3),1)||substr(lower(hex(randomblob(2))),2)||'-'||lower(hex(randomblob(6))) FROM messages;
DROP TABLE messages;
ALTER TABLE messages_new RENAME TO messages;
CREATE UNIQUE INDEX messages_idem ON messages (from_principal, idem) WHERE idem IS NOT NULL;
CREATE INDEX messages_queue ON messages (to_agent, state, created_at);
CREATE INDEX messages_pair ON messages (from_principal, to_agent, created_at);

CREATE UNIQUE INDEX messages_wire ON messages (wire_id);
CREATE INDEX messages_thread ON messages (thread_id, created_at);
CREATE TABLE mail_pause (
    agent_id TEXT PRIMARY KEY REFERENCES agents (id),
    at INTEGER NOT NULL,
    by TEXT NOT NULL
) STRICT;
