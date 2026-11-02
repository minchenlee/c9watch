-- C1a store, version 2 (plan C1a-15): what detection knows about an agent.
-- Every column is nullable, so rows from version 1 stay valid.

-- SessionIdentity::key() (`<provider>:<session id>`), set by detection.
ALTER TABLE agents ADD COLUMN session_key TEXT;
-- The machine the agent runs on (machines.id).
ALTER TABLE agents ADD COLUMN machine_id TEXT;
-- Engine lifecycle state (A:303); null for an observed agent, which has no
-- engine state.
ALTER TABLE agents ADD COLUMN state TEXT CHECK (state IN (
    'creating', 'starting', 'ready', 'working', 'blocked', 'paused', 'refreshing',
    'interrupted', 'retiring', 'retired', 'failed', 'orphaned'));
-- The detected session status, as the GUI shows it.
ALTER TABLE agents ADD COLUMN status TEXT CHECK (status IN (
    'working', 'needs_attention', 'waiting_for_input', 'connecting'));
ALTER TABLE agents ADD COLUMN cwd TEXT;                -- observed: the project path
ALTER TABLE agents ADD COLUMN name TEXT;
ALTER TABLE agents ADD COLUMN pid INTEGER;
ALTER TABLE agents ADD COLUMN started_at INTEGER;      -- unix ms, as the provider reports it
ALTER TABLE agents ADD COLUMN created_at INTEGER;      -- unix ms
ALTER TABLE agents ADD COLUMN last_seen INTEGER;       -- unix ms of the last tick that saw it

-- One detected row per session; a session that comes back gets its row back.
CREATE UNIQUE INDEX agents_detected_key ON agents (session_key) WHERE source = 'detected';
-- Engine-controlled rows are matched to detected sessions by provider_sid.
CREATE INDEX agents_provider_sid ON agents (provider, provider_sid);
