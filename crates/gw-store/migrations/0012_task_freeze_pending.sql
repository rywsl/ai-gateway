-- 0012_task_freeze_pending.sql - persistent retry queue for failed
-- task-freeze releases (R9 fix; WS-1 T18 sweeper consumes this table).
CREATE TABLE IF NOT EXISTS task_freeze_pending (
    id              BIGSERIAL   PRIMARY KEY,
    user_id         TEXT        NOT NULL,
    task_id         UUID        NOT NULL,
    frozen_micro    BIGINT      NOT NULL CHECK (frozen_micro > 0),
    attempt_count   INT         NOT NULL DEFAULT 0,
    last_error      TEXT        NULL,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS task_freeze_pending_task_idx
    ON task_freeze_pending (task_id);

CREATE INDEX IF NOT EXISTS task_freeze_pending_due_idx
    ON task_freeze_pending (next_attempt_at, id);

CREATE INDEX IF NOT EXISTS task_freeze_pending_user_task_idx
    ON task_freeze_pending (user_id, task_id);
