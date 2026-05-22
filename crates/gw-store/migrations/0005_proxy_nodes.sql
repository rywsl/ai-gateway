-- 0005_proxy_nodes.sql — proxy_nodes entity for tunnel decision (spec ADR-T-3).
--
-- Mirrors the (id, payload, version, updated_at, deleted_at) shape of every
-- other entity table in 0001_initial.sql. JSONB carries the full
-- ProxyNode payload including AES-256-GCM ciphertext of any per-node auth
-- token (encrypted under the active KEK; see crates/gw-store/src/encrypt.rs).

CREATE TABLE IF NOT EXISTS proxy_nodes (
    id          TEXT        PRIMARY KEY,
    payload     JSONB       NOT NULL,
    version     BIGINT      NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ NULL
);

