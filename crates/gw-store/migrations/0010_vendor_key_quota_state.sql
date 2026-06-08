-- 0010_vendor_key_quota_state.sql
-- Persist slow-changing vendor quota snapshots outside payload so routing can
-- cold-load and deselect temporarily exhausted keys without parsing vendor
-- health state from request-local errors.

ALTER TABLE vendor_keys
    ADD COLUMN IF NOT EXISTS quota_availability TEXT NOT NULL DEFAULT 'unknown',
    ADD COLUMN IF NOT EXISTS quota_remaining BIGINT NULL,
    ADD COLUMN IF NOT EXISTS quota_limit BIGINT NULL,
    ADD COLUMN IF NOT EXISTS quota_reset_at TIMESTAMPTZ NULL,
    ADD COLUMN IF NOT EXISTS quota_observed_at TIMESTAMPTZ NULL,
    ADD COLUMN IF NOT EXISTS quota_source TEXT NULL,
    ADD CONSTRAINT vendor_keys_quota_availability_chk
        CHECK (quota_availability IN ('unknown', 'available', 'exhausted'));

CREATE INDEX IF NOT EXISTS idx_vendor_keys_quota_reset
    ON vendor_keys (quota_availability, quota_reset_at)
    WHERE deleted_at IS NULL;
