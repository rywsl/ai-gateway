-- 0004_capabilities.sql — model + vendor_key capability metadata for T3-C.
-- See docs/ROADMAP.md (capability resolver / exclusive_capability_match).
--
-- Adds the per-model entity that T3-C's CapabilityResolver reads via
-- EntityStore::model_or_load (loader landing in T3-C). The table follows
-- the standard (id, payload, version, updated_at, deleted_at) shape used
-- by every other entity table in 0001_initial.sql.

CREATE TABLE IF NOT EXISTS models (
    id          TEXT        PRIMARY KEY,
    payload     JSONB       NOT NULL,
    version     BIGINT      NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ NULL
);

-- Per-key capability advertisement. Existing rows default to empty/false
-- so prior payloads remain valid without backfill.
ALTER TABLE vendor_keys
    ADD COLUMN IF NOT EXISTS forced_capabilities JSONB NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS exclusive_capability_match BOOLEAN NOT NULL DEFAULT FALSE;
