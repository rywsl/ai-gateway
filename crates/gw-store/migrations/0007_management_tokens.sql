-- 0007_management_tokens.sql — ManagementTokenEntry persistence (B2 / ADR-B2-1).
-- 0005_proxy_nodes 与 0006_auth_configs 已先占编号。

CREATE TABLE IF NOT EXISTS management_tokens (
    id          TEXT        PRIMARY KEY,
    payload     JSONB       NOT NULL,
    version     BIGINT      NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ NULL
);

CREATE INDEX IF NOT EXISTS management_tokens_user_id_idx
    ON management_tokens ((payload->>'user_id'));
