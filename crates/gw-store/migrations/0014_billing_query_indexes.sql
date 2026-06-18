-- 0014_billing_query_indexes.sql - SLQ3 Rust portion. Covering indexes for
-- gateway billing and routing hot-path lookups.
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_vendor_keys_active_lookup
    ON vendor_keys (id)
    INCLUDE (version, quota_availability, quota_reset_at, quota_remaining, quota_limit)
    WHERE deleted_at IS NULL;

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_task_records_user_status_created
    ON task_records (user_id, status, created_at DESC)
    INCLUDE (task_id, api_key_id, frozen_micro, actual_micro, settled_at);

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_task_records_api_key_status_created
    ON task_records (api_key_id, status, created_at DESC)
    INCLUDE (task_id, user_id, frozen_micro, actual_micro, settled_at);
