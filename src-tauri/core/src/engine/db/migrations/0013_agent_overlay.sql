-- E1: the spawn overlay of an engine agent (agent.create overlay, as
-- the engine checked it, JSON AgentOverlay). NULL for a bare create and
-- for detected and imported rows.
ALTER TABLE agents ADD COLUMN overlay TEXT CHECK (overlay IS NULL OR (json_valid(overlay) AND json_type(overlay) = 'object'));
