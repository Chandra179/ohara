-- Entity garbage-collection candidates (§8 Stage 4).
-- A candidate records the first observation that an entity had no MENTIONS
-- edges. Merge-audit rows intentionally keep their referenced entities alive.
CREATE TABLE IF NOT EXISTS entity_gc_candidates (
    entity_id   TEXT PRIMARY KEY REFERENCES entities(entity_id) ON DELETE CASCADE,
    zero_since  TIMESTAMP NOT NULL,
    detected_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE INDEX IF NOT EXISTS idx_entity_gc_due ON entity_gc_candidates(zero_since);
