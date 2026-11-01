-- C1a store, version 9 (D2, P1-3): what an unconfirmed kill left. Nullable,
-- so rows from version 8 stay valid.

-- JSON array of `{pid, start, pgid}`, at most 64: the processes of an
-- `orphaned` agent that still ran when no kill could confirm its end. A
-- retire kills each again by pid and start time, and tombstones the row
-- only once every one is gone. NULL: none.
ALTER TABLE agent_state ADD COLUMN survivors TEXT;
