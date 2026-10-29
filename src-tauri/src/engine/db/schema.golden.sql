-- user_version 2
-- index agents_detected_key
CREATE UNIQUE INDEX agents_detected_key ON agents (session_key) WHERE source = 'detected';
-- index agents_provider_sid
CREATE INDEX agents_provider_sid ON agents (provider, provider_sid);
-- index events_ts
CREATE INDEX events_ts ON events (ts);
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
    'working', 'needs_attention', 'waiting_for_input', 'connecting')), cwd TEXT, name TEXT, pid INTEGER, started_at INTEGER, created_at INTEGER, last_seen INTEGER) STRICT;
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
-- table machines
CREATE TABLE machines (
    id      TEXT PRIMARY KEY,
    label   TEXT,
    is_self INTEGER NOT NULL DEFAULT 0
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
