-- 0006_auth_configs.sql — AuthConfig persistence (G2 / ADR-G2-1).
-- 0005_proxy_nodes already exists; 0007_management_tokens is owned by the
-- blockers plan.
--
-- Mirrors the (id, payload, version, updated_at, deleted_at) shape of every
-- other entity table. The `global` row is NOT seeded by this migration —
-- the Go management plane writes it via the admin UI / init script.

CREATE TABLE IF NOT EXISTS auth_configs (
    id          TEXT        PRIMARY KEY,
    payload     JSONB       NOT NULL,
    version     BIGINT      NOT NULL DEFAULT 1,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ NULL
);
