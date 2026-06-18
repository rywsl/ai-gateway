# WS-14 Migration Runbook

Authoritative plan: `docs/superpowers/plans/2026-06-17-WS-14-migration-coordination.md`.

## Rust Migrations

| File | Rollback | Expected Duration | Failure Mode |
|---|---|---:|---|
| `0012_task_freeze_pending.sql` | `psql "$PG_DSN" -f crates/gw-store/migrations/0012_task_freeze_pending.down.sql` | < 1 min | Rollback deletes pending task-freeze recovery rows; only use before sweeper cutover. |
| `0013_pricings_extended.sql` | `psql "$PG_DSN" -f crates/gw-store/migrations/0013_pricings_extended.down.sql` | < 1 min | Down drops explicit prices for new dimensions; export any backfilled values first. |
| `0014_billing_query_indexes.sql` | `psql "$PG_DSN" -f crates/gw-store/migrations/0014_billing_query_indexes.down.sql` | Size dependent | `CREATE/DROP INDEX CONCURRENTLY` can fail if cancelled; rerun is safe. |
| `0015_quota_state_outbox.sql` | `psql "$PG_DSN" -f crates/gw-store/migrations/0015_quota_state_outbox.down.sql` | < 1 min | Rollback deletes quota-state outbox rows; only use before outbox worker cutover. |

## Validation

Run `scripts/check-migration-number.sh` and `scripts/verify-migrations-roundtrip.sh --static` before merge. Full schema byte-equivalence uses a staging snapshot:

```bash
scripts/verify-migrations-roundtrip.sh "$PG_DSN"
```

The e2e migration runner must skip `*.down.sql` sidecars. Production order remains Rust first, wait at least 10 minutes after Rust readiness, then Go.
