#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ -n "${BODY:-}" ]]; then
  pr_body="$BODY"
else
  pr_number="${1:-${PR_NUMBER:?usage: BODY=... $0 or $0 <pr_number>}}"
  pr_body="$(gh pr view "$pr_number" --json body --jq .body)"
fi

missing=0
check_line() {
  local label="$1"
  local pattern="$2"
  if ! grep -qE "$pattern" <<<"$pr_body"; then
    echo "ERROR: PR description missing or malformed line: '$label'"
    missing=1
  fi
}

check_line "Flag" '^Flag:[[:space:]]+([a-z][a-z0-9_]*|none)$'
check_line "Default" '^Default:[[:space:]]+\S+$'
check_line "Switch-Date" '^Switch-Date:[[:space:]]+(N\+[0-9]+|none|[0-9]{4}-[0-9]{2}-[0-9]{2})$'
check_line "Rollback" '^Rollback:[[:space:]]+.+$'

if [[ "$missing" -ne 0 ]]; then
  exit 1
fi

flag="$(grep -E '^Flag:' <<<"$pr_body" | awk '{print $2}' | tail -n 1)"
if [[ "$flag" != "none" && ! -f "flags/$flag.yaml" ]]; then
  echo "ERROR: Flag: $flag declared but flags/$flag.yaml does not exist"
  exit 1
fi

echo "PR description WS-15 block: OK"
