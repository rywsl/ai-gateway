#!/usr/bin/env bash
# WS-14: migration-number and down-script gate.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 2

plan="docs/superpowers/plans/2026-06-17-WS-14-migration-coordination.md"
test -f "$plan" || { echo "FATAL: missing $plan" >&2; exit 2; }

required_rust="
crates/gw-store/migrations/0012_task_freeze_pending.sql
crates/gw-store/migrations/0013_pricings_extended.sql
crates/gw-store/migrations/0014_billing_query_indexes.sql
crates/gw-store/migrations/0015_quota_state_outbox.sql
"
allowed_rust="$required_rust"

fail=0
for f in $required_rust; do
  if [[ ! -f "$f" ]]; then
    echo "FAIL: required WS-14 Rust migration missing: $f" >&2
    fail=1
  fi
  down="${f%.sql}.down.sql"
  if [[ ! -f "$down" ]]; then
    echo "FAIL: required WS-14 Rust down sidecar missing: $down" >&2
    fail=1
  fi
done

dups=$(find crates/gw-store/migrations -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9]_*.sql' ! -name '*.down.sql' -exec basename {} \; |
  awk '{print substr($0,1,4)}' | sort | uniq -d)
for p in $dups; do
  list=$(find crates/gw-store/migrations -maxdepth 1 -type f -name "${p}_*.sql" ! -name '*.down.sql' -exec basename {} \; | sort)
  case "$p:$list" in
    $'0004:0004_capabilities.sql\n0004_task_worker.sql') ;;
    *)
      echo "FAIL: Rust duplicate migration prefix $p outside whitelist:" >&2
      printf '%s\n' "$list" >&2
      fail=1
      ;;
  esac
done

base="${BASE_REF:-${CI_MERGE_REQUEST_TARGET_BRANCH_NAME:-}}"
if [[ -n "$base" ]] && git rev-parse --verify "$base" >/dev/null 2>&1; then
  changed=$(git diff --name-only --diff-filter=A "$base"...HEAD -- 'crates/gw-store/migrations/*.sql' || true)
elif git rev-parse --verify origin/main >/dev/null 2>&1; then
  changed=$(git diff --name-only --diff-filter=A origin/main...HEAD -- 'crates/gw-store/migrations/*.sql' || true)
else
  changed=""
fi

for f in $changed; do
  [[ "$f" == *.down.sql ]] && continue
  if ! printf '%s\n' "$allowed_rust" | grep -qFx "$f"; then
    echo "FAIL: $f not listed in WS-14 section 1.2" >&2
    fail=1
  fi
done

if (( fail )); then
  exit 1
fi
echo "[check-migration-number] OK"
