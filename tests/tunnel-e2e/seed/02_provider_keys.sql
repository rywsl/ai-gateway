-- Phase 1 keeps runtime seed data in pytest fixtures so each scenario can
-- switch VendorKey.tunnel_preference and ProxyNode rows independently.
--
-- The gateway reads estimate_pricings at request admission, but the historical
-- migration set does not own the table yet. Create the standard entity shape
-- here so tunnel-e2e can seed the runtime contract without changing production
-- migrations in this harness-only commit.
CREATE TABLE IF NOT EXISTS estimate_pricings (
    id          TEXT        PRIMARY KEY,
    payload     JSONB       NOT NULL,
    version     BIGINT      NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ NULL
);
