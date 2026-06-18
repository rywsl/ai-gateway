-- 0015_quota_state_outbox.down.sql

DROP INDEX IF EXISTS quota_state_outbox_due_idx;
DROP TABLE IF EXISTS quota_state_outbox;
