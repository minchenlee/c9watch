-- C1a store, version 9 (D2, P1-3): what an unconfirmed kill left. Nullable,
-- so rows from version 8 stay valid.

-- JSON `{"v":1,"overflow":bool,"list":[{pid, start, pgid}]}`, at most 64
-- in the list: the processes of an `orphaned` agent that still ran when no
-- kill could confirm its end. `overflow`: there were more. A retire kills
-- each again by pid and start time, and tombstones the row only once every
-- one is gone and there was no overflow. NULL: none.
ALTER TABLE agent_state ADD COLUMN survivors TEXT;
