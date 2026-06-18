#!/usr/bin/env bash
# WS-12 T-06: require protocol version bumps for added Rust contract fields.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 2

base="${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$(git merge-base origin/main HEAD 2>/dev/null || true)}"
if [[ -z "$base" ]]; then
  echo "[check-rustpact-pv] no merge base; skipping"
  exit 0
fi

added=0
while IFS= read -r line; do
  [[ "$line" =~ ^\+\+\+ ]] && continue
  if [[ "$line" =~ ^\+[[:space:]]+pub[[:space:]]+[A-Za-z_][A-Za-z0-9_]*: ]]; then
    ((added++))
  fi
done < <(git diff --unified=0 "$base"...HEAD -- crates/gw-ledger/src crates/gw-core/src crates/gw-task/src docs/go-contracts 2>/dev/null || true)

if (( added == 0 )); then
  echo "[check-rustpact-pv] no new Rust contract fields - OK"
  exit 0
fi

bump_lines="$(git diff "$base"...HEAD -- crates/gw-runtime/src/contracts_version.rs 2>/dev/null | grep -E '^[+-].*RUSTPACT_PROTOCOL_VERSION' | wc -l | tr -d ' ')"
if (( bump_lines >= 2 )); then
  echo "[check-rustpact-pv] OK - $added field(s), protocol version bumped"
  exit 0
fi

echo "[check-rustpact-pv] FAIL - $added new field(s) but RUSTPACT_PROTOCOL_VERSION was not bumped" >&2
exit 1
