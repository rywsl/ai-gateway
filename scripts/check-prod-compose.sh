#!/usr/bin/env bash
# WS-12 T-04 mirror: Rust repo currently has no production compose manifest.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 2

status=0
while IFS= read -r file; do
  while IFS=: read -r line text; do
    [[ -z "${line:-}" ]] && continue
    echo "VIOLATION: $file:$line forbidden demo bootstrap env in production manifest: $text" >&2
    status=1
  done < <(
    grep -nE '^[[:space:]]*(ALM_E2E_LICENSE_BOOTSTRAP|RUN_DEV_SEED|ALM_DEV_ADMIN_BOOTSTRAP)[[:space:]]*:|^[[:space:]]*-[[:space:]]*name:[[:space:]]*(ALM_E2E_LICENSE_BOOTSTRAP|RUN_DEV_SEED|ALM_DEV_ADMIN_BOOTSTRAP)\b' "$file" 2>/dev/null |
      grep -vE '^[0-9]+:[[:space:]]*#' || true
  )
done < <(
  find . \
    \( -path './target' -o -path './.git' -o -path './.cache' \) -prune -o \
    \( -name 'docker-compose.prod*.yml' -o -name 'docker-compose.prod*.yaml' -o -path './deploy/k8s/*.yaml' -o -path './deploy/k8s/*.yml' \) \
    -type f -print | sort
)

if (( status )); then
  echo "[check-prod-compose] FAIL" >&2
  exit "$status"
fi
echo "[check-prod-compose] OK"
