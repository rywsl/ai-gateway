-- 0012_task_freeze_pending.down.sql
DROP INDEX IF EXISTS task_freeze_pending_user_task_idx;
DROP INDEX IF EXISTS task_freeze_pending_due_idx;
DROP INDEX IF EXISTS task_freeze_pending_task_idx;
DROP TABLE IF EXISTS task_freeze_pending;
