-- C1a store, version 4 (plan C1a-27): what a drain leaves for the next
-- engine. Both columns are nullable, so rows from version 3 stay valid.

-- Why the row is in its state: `drain` for a `paused` or `interrupted`
-- row that a drain left (A:229-231).
ALTER TABLE agents ADD COLUMN state_reason TEXT;
-- JSON array: the argv that resumes the agent's session (A:1038), recorded
-- by the drain for the boot resume (C1a-28).
ALTER TABLE agents ADD COLUMN resume_argv TEXT;
