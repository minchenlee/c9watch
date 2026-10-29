-- C1a store, version 3 (plan C1a-18): rows imported from the legacy
-- `~/.claude/c9watch` files.

-- The agent that owns this one (A:351). Set from a legacy adoption record.
ALTER TABLE agents ADD COLUMN parent_id TEXT REFERENCES agents (id);

-- One imported row per legacy session, so a re-import updates it.
CREATE UNIQUE INDEX agents_legacy_sid ON agents (provider, provider_sid)
    WHERE source = 'legacy_import';

-- Legacy inbox events already imported. Kept apart from `events`, which
-- retention prunes, so a pruned event is not imported again.
CREATE TABLE legacy_events (
    event_id    TEXT PRIMARY KEY,
    imported_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
