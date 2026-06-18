#!/usr/bin/env bash
# WS-12 T-05: baseline-gate money-sensitive Rust `as` casts.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 2

baseline="scripts/check-money-as-cast.baseline"
tmp="$(mktemp "${TMPDIR:-/tmp}/money-cast.XXXXXX")"
trap 'rm -f "$tmp"' EXIT

while IFS=: read -r file line _; do
  [[ -z "${file:-}" ]] && continue
  echo "$file:$line:money-cast" >> "$tmp"
done < <(
  rg -n '\bas\s+(i64|u64|f64)\b' crates/gw-billing/src crates/gw-ledger/src crates/gw-store/src \
    --glob '*.rs' --glob '!**/tests/**' --glob '!**/benches/**' --glob '!**/examples/**' 2>/dev/null || true
)

unknown=0
while IFS= read -r finding; do
  [[ -z "$finding" ]] && continue
  if ! grep -qxF "$finding" "$baseline"; then
    echo "VIOLATION: $finding new money-sensitive cast; use checked conversion or update baseline with review" >&2
    unknown=$((unknown + 1))
  fi
done < "$tmp"

if (( unknown )); then
  echo "[check-money-as-cast] FAIL - $unknown new finding(s)" >&2
  exit 1
fi
echo "[check-money-as-cast] OK ($(wc -l < "$tmp" | tr -d ' ') current baseline finding(s), no new findings)"
