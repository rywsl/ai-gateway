-- 0014_billing_query_indexes.down.sql
DROP INDEX CONCURRENTLY IF EXISTS idx_task_records_api_key_status_created;
DROP INDEX CONCURRENTLY IF EXISTS idx_task_records_user_status_created;
DROP INDEX CONCURRENTLY IF EXISTS idx_vendor_keys_active_lookup;
