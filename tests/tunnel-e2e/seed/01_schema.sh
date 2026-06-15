#!/usr/bin/env bash
set -euo pipefail

PGHOST="${PGHOST:-postgres}"
PGUSER="${PGUSER:-postgres}"
PGDATABASE="${PGDATABASE:-cnexus}"

echo "[schema-applier] waiting for postgres..."
for _ in $(seq 1 30); do
  if pg_isready -h "$PGHOST" -U "$PGUSER" -d "$PGDATABASE" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "[schema-applier] applying gw-store migrations..."
for f in $(find /migrations -maxdepth 1 -type f -name '*.sql' | sort); do
  echo "  - $f"
  psql -h "$PGHOST" -U "$PGUSER" -d "$PGDATABASE" -v ON_ERROR_STOP=1 -f "$f"
done

if [ -f /seed/02_provider_keys.sql ]; then
  echo "[schema-applier] applying tunnel-e2e seed..."
  psql -h "$PGHOST" -U "$PGUSER" -d "$PGDATABASE" -v ON_ERROR_STOP=1 -f /seed/02_provider_keys.sql
fi

echo "[schema-applier] done."
