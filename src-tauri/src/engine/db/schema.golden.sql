-- user_version 7
-- index agents_detected_key
CREATE UNIQUE INDEX agents_detected_key ON agents (session_key) WHERE source = 'detected';
-- index agents_legacy_sid
CREATE UNIQUE INDEX agents_legacy_sid ON agents (provider, provider_sid)
    WHERE source = 'legacy_import';
-- index agents_provider_sid
CREATE INDEX agents_provider_sid ON agents (provider, provider_sid);
-- index events_ts
CREATE INDEX events_ts ON events (ts);
-- index gateway_tokens_expiry
CREATE INDEX gateway_tokens_expiry ON gateway_tokens (expires_at);
-- index message_status_msg
CREATE INDEX message_status_msg ON message_status (msg_id, seq);
-- index messages_idem
CREATE UNIQUE INDEX messages_idem ON messages (from_principal, idem) WHERE idem IS NOT NULL;
-- index messages_pair
CREATE INDEX messages_pair ON messages (from_principal, to_agent, created_at);
-- index messages_queue
CREATE INDEX messages_queue ON messages (to_agent, state, created_at);
-- index messages_thread
CREATE INDEX messages_thread ON messages (thread_id, created_at);
-- index messages_wire
CREATE UNIQUE INDEX messages_wire ON messages (wire_id);
-- index ops_open
CREATE INDEX ops_open ON ops (done_at) WHERE done_at IS NULL;
-- index session_alias_agent
CREATE INDEX session_alias_agent ON session_alias (agent_id, generation);
-- table agent_state
CREATE TABLE agent_state (
    agent_id        TEXT PRIMARY KEY REFERENCES agents (id),
    gen             INTEGER NOT NULL,
    pty_child_pid   INTEGER,
    pty_child_start INTEGER
) STRICT;
-- table agents
CREATE TABLE agents (
    id                       TEXT PRIMARY KEY,           -- agt_<ulid>
    generation               INTEGER NOT NULL DEFAULT 0, -- +1 on spawn, resume, refresh, takeover
    mode                     TEXT CHECK (mode IN ('managed', 'free')), -- null for observed
    role                     TEXT NOT NULL CHECK (role IN ('human', 'orch', 'worker', 'observed')),
    control                  TEXT NOT NULL DEFAULT 'none' CHECK (control IN ('pty', 'none')),
    provider                 TEXT NOT NULL,              -- adapter kind: claude|codex|...
    provider_sid             TEXT,                       -- current generation; history in session_alias
    source                   TEXT NOT NULL CHECK (source IN ('detected', 'engine', 'legacy_import')),
    created_by               TEXT,                       -- principal
    gone_at                  INTEGER,                    -- unix ms; null while alive
    resume_count             INTEGER NOT NULL DEFAULT 0, -- engine_restart resumes not counted
    interrupted_note_pending INTEGER NOT NULL DEFAULT 0
, session_key TEXT, machine_id TEXT, state TEXT CHECK (state IN (
    'creating', 'starting', 'ready', 'working', 'blocked', 'paused', 'refreshing',
    'interrupted', 'retiring', 'retired', 'failed', 'orphaned')), status TEXT CHECK (status IN (
    'working', 'needs_attention', 'waiting_for_input', 'connecting')), cwd TEXT, name TEXT, pid INTEGER, started_at INTEGER, created_at INTEGER, last_seen INTEGER, parent_id TEXT REFERENCES agents (id), state_reason TEXT, resume_argv TEXT) STRICT;
-- table audit
CREATE TABLE audit (
    seq          INTEGER PRIMARY KEY,
    ts           INTEGER NOT NULL,
    principal    TEXT NOT NULL,
    origin       TEXT,
    method       TEXT NOT NULL,
    target       TEXT,
    decision     TEXT NOT NULL,
    reason       TEXT,
    payload_hash TEXT
) STRICT;
-- table dedupe
CREATE TABLE dedupe (
    peer   TEXT NOT NULL,
    msg_id TEXT NOT NULL,
    ts     INTEGER NOT NULL,
    PRIMARY KEY (peer, msg_id)
) STRICT, WITHOUT ROWID;
-- table events
CREATE TABLE events (
    seq     INTEGER PRIMARY KEY,
    epoch   TEXT NOT NULL,
    topic   TEXT NOT NULL,
    subject TEXT,
    origin  TEXT,                                        -- JSON
    payload TEXT,                                        -- JSON
    ts      INTEGER NOT NULL
) STRICT;
-- table gateway_tokens
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
-- table legacy_events
CREATE TABLE legacy_events (
    event_id    TEXT PRIMARY KEY,
    imported_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
-- table machines
CREATE TABLE machines (
    id      TEXT PRIMARY KEY,
    label   TEXT,
    is_self INTEGER NOT NULL DEFAULT 0
) STRICT;
-- table mail_pause
CREATE TABLE mail_pause (
    agent_id TEXT PRIMARY KEY REFERENCES agents (id),
    at INTEGER NOT NULL,
    by TEXT NOT NULL
) STRICT;
-- table message_status
CREATE TABLE message_status (
    seq    INTEGER PRIMARY KEY,
    msg_id TEXT NOT NULL REFERENCES messages (id),
    state  TEXT NOT NULL,
    at     INTEGER NOT NULL,
    detail TEXT
) STRICT;
-- table messages
CREATE TABLE "messages" (
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
-- table meta
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT, WITHOUT ROWID;
-- table ops
CREATE TABLE ops (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    phase      TEXT NOT NULL,
    payload    TEXT,                                     -- JSON
    created_at INTEGER NOT NULL,
    done_at    INTEGER
) STRICT;
-- table session_alias
CREATE TABLE session_alias (
    agent_id        TEXT NOT NULL REFERENCES agents (id),
    generation      INTEGER NOT NULL,
    provider_sid    TEXT NOT NULL,
    transcript_path TEXT,
    bound_at        INTEGER NOT NULL
) STRICT;
