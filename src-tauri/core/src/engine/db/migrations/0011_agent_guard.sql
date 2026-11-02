-- D30: a write fence on agent.create (caller data, realpaths, JSON
-- AgentGuard), and the Claude Code version and capability tokens of the
-- spawn (JSON array). All null for detected and imported rows.
ALTER TABLE agents ADD COLUMN guard TEXT CHECK (guard IS NULL OR json_valid(guard));
ALTER TABLE agents ADD COLUMN cc_version TEXT;
ALTER TABLE agents ADD COLUMN capabilities TEXT CHECK (capabilities IS NULL OR (json_valid(capabilities) AND json_type(capabilities) = 'array'));
