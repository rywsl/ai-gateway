CREATE TABLE IF NOT EXISTS task_records (
    task_id         UUID        PRIMARY KEY,
    user_id         TEXT        NOT NULL,
    api_key_id      TEXT        NOT NULL,
    kind            TEXT        NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'pending',
    input_payload   JSONB,
    output_payload  JSONB,
    model           TEXT,
    vendor_key_id   TEXT,
    frozen_micro    BIGINT      NOT NULL DEFAULT 0,
    actual_micro    BIGINT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    settled_at      TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_task_records_user_status
    ON task_records (user_id, status);

CREATE INDEX IF NOT EXISTS idx_task_records_api_key_id
    ON task_records (api_key_id);

CREATE INDEX IF NOT EXISTS idx_task_records_created_at_desc
    ON task_records (created_at DESC);

CREATE INDEX IF NOT EXISTS idx_task_records_active_status
    ON task_records (status)
    WHERE status IN ('pending', 'running');

CREATE TABLE IF NOT EXISTS worker_registrations (
    worker_id       TEXT        PRIMARY KEY,
    last_heartbeat  TIMESTAMPTZ NOT NULL DEFAULT now(),
    status          TEXT        NOT NULL DEFAULT 'active',
    metadata        JSONB
);

CREATE INDEX IF NOT EXISTS idx_worker_registrations_last_heartbeat
    ON worker_registrations (last_heartbeat DESC);

CREATE INDEX IF NOT EXISTS idx_worker_registrations_status
    ON worker_registrations (status);
