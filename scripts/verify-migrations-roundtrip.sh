#!/usr/bin/env bash
# WS-14: Rust migration roundtrip verifier. Use --static for local/CI lint only.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 2

mode="${1:---static}"
ups="
crates/gw-store/migrations/0012_task_freeze_pending.sql
crates/gw-store/migrations/0013_pricings_extended.sql
crates/gw-store/migrations/0014_billing_query_indexes.sql
crates/gw-store/migrations/0015_quota_state_outbox.sql
"
downs="
crates/gw-store/migrations/0015_quota_state_outbox.down.sql
crates/gw-store/migrations/0014_billing_query_indexes.down.sql
crates/gw-store/migrations/0013_pricings_extended.down.sql
crates/gw-store/migrations/0012_task_freeze_pending.down.sql
"

static_check() {
  for f in $ups $downs; do
    test -f "$f" || { echo "FAIL: missing $f" >&2; return 1; }
  done
  if grep -n '\.down\.sql' tests/e2e/run_migrations.sh | grep -q 'continue'; then
    :
  else
    echo "FAIL: tests/e2e/run_migrations.sh must skip *.down.sql sidecars" >&2
    return 1
  fi
}

if [[ "$mode" == "--static" ]]; then
  static_check
  echo "[verify-migrations-roundtrip] static OK"
  exit 0
fi

dsn="${1:-${PG_DSN:-${DATABASE_URL:-}}}"
if [[ -z "$dsn" ]]; then
  echo "usage: $0 --static | <postgres-dsn>" >&2
  exit 2
fi
command -v psql >/dev/null || { echo "FATAL: psql required" >&2; exit 2; }
command -v pg_dump >/dev/null || { echo "FATAL: pg_dump required" >&2; exit 2; }

static_check
tmp="${TMPDIR:-/tmp}/ws14-rust-roundtrip.$$"
mkdir -p "$tmp"
trap 'rm -rf "$tmp"' EXIT

pg_dump --schema-only "$dsn" > "$tmp/baseline.sql"
for f in $ups; do psql "$dsn" -v ON_ERROR_STOP=1 -f "$f"; done
pg_dump --schema-only "$dsn" > "$tmp/up.sql"
for f in $downs; do psql "$dsn" -v ON_ERROR_STOP=1 -f "$f"; done
pg_dump --schema-only "$dsn" > "$tmp/down.sql"
for f in $ups; do psql "$dsn" -v ON_ERROR_STOP=1 -f "$f"; done
pg_dump --schema-only "$dsn" > "$tmp/up2.sql"

diff -u "$tmp/up.sql" "$tmp/up2.sql" >/dev/null
diff -u "$tmp/baseline.sql" "$tmp/down.sql" >/dev/null
echo "[verify-migrations-roundtrip] PASS"
