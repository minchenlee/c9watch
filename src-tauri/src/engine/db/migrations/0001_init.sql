-- C1a store, version 1 (A:340-455, plan C1a-11).
-- Tasks, plans and approvals come with C1b; mail is version 5.
-- STRICT tables: a wrong type is an error, not a silent coercion.

-- Engine-private key/value facts about this database file, such as its
-- epoch. Not part of the A:340 model; the epoch has to live in the file it
-- names, so a moved-aside db takes its epoch with it.
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT, WITHOUT ROWID;

-- id is the machine's ed25519 public key (C1a-12).
CREATE TABLE machines (
    id      TEXT PRIMARY KEY,
    label   TEXT,
    is_self INTEGER NOT NULL DEFAULT 0
) STRICT;

-- No token_hash (D28 Q6=B): generation identity lives in agent_state.
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
) STRICT;

-- One row per bound provider session (D22.13).
CREATE TABLE session_alias (
    agent_id        TEXT NOT NULL REFERENCES agents (id),
    generation      INTEGER NOT NULL,
    provider_sid    TEXT NOT NULL,
    transcript_path TEXT,
    bound_at        INTEGER NOT NULL
) STRICT;
CREATE INDEX session_alias_agent ON session_alias (agent_id, generation);

-- The current generation's identity: the PTY child by pid + start time.
CREATE TABLE agent_state (
    agent_id        TEXT PRIMARY KEY REFERENCES agents (id),
    gen             INTEGER NOT NULL,
    pty_child_pid   INTEGER,
    pty_child_start INTEGER
) STRICT;

-- Operation journal (D22). Open op = done_at IS NULL; finished or undone
-- on boot before the engine accepts mutations.
CREATE TABLE ops (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL,
    phase      TEXT NOT NULL,
    payload    TEXT,                                     -- JSON
    created_at INTEGER NOT NULL,
    done_at    INTEGER
) STRICT;
CREATE INDEX ops_open ON ops (done_at) WHERE done_at IS NULL;

-- Event log; written in the same transaction as the state change (D3).
CREATE TABLE events (
    seq     INTEGER PRIMARY KEY,
    epoch   TEXT NOT NULL,
    topic   TEXT NOT NULL,
    subject TEXT,
    origin  TEXT,                                        -- JSON
    payload TEXT,                                        -- JSON
    ts      INTEGER NOT NULL
) STRICT;
CREATE INDEX events_ts ON events (ts);

-- Never pruned in v1.
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

-- Replay window; used only with C2a (D16).
CREATE TABLE dedupe (
    peer   TEXT NOT NULL,
    msg_id TEXT NOT NULL,
    ts     INTEGER NOT NULL,
    PRIMARY KEY (peer, msg_id)
) STRICT, WITHOUT ROWID;
