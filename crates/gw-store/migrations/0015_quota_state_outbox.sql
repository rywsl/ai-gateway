-- 0015_quota_state_outbox.sql - R23 / REV-FINAL-W12.
-- Durable Redis invalidation/event outbox for quota-state writes.

CREATE TABLE IF NOT EXISTS quota_state_outbox (
    id BIGSERIAL PRIMARY KEY,
    vendor_key_id TEXT NOT NULL,
    payload JSONB NOT NULL,
    status TEXT NOT NULL
        CHECK (status IN ('pending', 'emitted_xadd', 'emitted_del', 'committed', 'failed')),
    attempt INT NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS quota_state_outbox_due_idx
    ON quota_state_outbox (next_attempt_at, id)
    WHERE status IN ('pending', 'emitted_xadd', 'emitted_del');
