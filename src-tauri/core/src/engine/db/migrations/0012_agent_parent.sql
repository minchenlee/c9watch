-- E5: an agent's children, found by parent_id (lineage limits, the
-- retire of a parent's descendants).
CREATE INDEX agents_parent ON agents (parent_id) WHERE parent_id IS NOT NULL;
