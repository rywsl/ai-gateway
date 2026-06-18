#!/usr/bin/env bash
# WS-10 T08 / WS-12 T-08: ban high-cardinality or PII label names on Prometheus metrics.
# Keep BANNED identical across AILinkMax-Go and ailinkmax-gateway.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# M13 canonical banned set. Do not add bounded enum labels such as
# result, op, path, attempt_index, or model_id.
BANNED=(request_id api_key_id user_id ip ip_address email token session_id jti refresh_token access_token hash)
ALLOW_RE='//[[:space:]]*(allow-label|metric-label):[[:space:]]+[^[:space:]]+[[:space:]]+reason="[^"]{10,}"'

fail=0

allow_near() {
  local file="$1"
  local line="$2"
  local start=$((line - 3))
  if (( start < 1 )); then start=1; fi
  sed -n "${start},${line}p" "$file" 2>/dev/null | grep -Eq "$ALLOW_RE"
}

scan_go_file() {
  local file="$1"
  while IFS=: read -r line _; do
    [[ -z "$line" ]] && continue
    local start=$((line - 2))
    local end=$((line + 12))
    if (( start < 1 )); then start=1; fi
    local window
    window="$(sed -n "${start},${end}p" "$file")"
    for label in "${BANNED[@]}"; do
      if grep -Eq "\"${label}\"" <<<"$window"; then
        if allow_near "$file" "$line"; then
          continue
        fi
        echo "VIOLATION: ${file}:${line} metric uses banned label '${label}'"
        fail=1
      fi
    done
  done < <(grep -nE 'New(Counter|Gauge|Histogram|Summary)Vec\b' "$file" 2>/dev/null || true)
}

scan_rust_file() {
  local file="$1"
  while IFS=: read -r line _; do
    [[ -z "$line" ]] && continue
    local start="$line"
    local end=$((line + 12))
    local window
    window="$(
      sed -n "${start},${end}p" "$file" |
        awk '{ print; if ($0 ~ /(\)|^|[[:space:]])\.(increment|record|set)\(/) exit }'
    )"
    for label in "${BANNED[@]}"; do
      if grep -Eq "\"${label}\"[[:space:]]*(=>|,)" <<<"$window"; then
        if allow_near "$file" "$line"; then
          continue
        fi
        echo "VIOLATION: ${file}:${line} metric uses banned label '${label}'"
        fail=1
      fi
    done
  done < <(grep -nE 'metrics::(counter|gauge|histogram)!|register_(int_counter|counter|gauge|histogram|int_gauge)_vec!' "$file" 2>/dev/null || true)
}

if [[ -d shared || -d modules || -d binaries ]]; then
  while IFS= read -r file; do
    scan_go_file "$file"
  done < <(find shared modules binaries -name '*.go' ! -name '*_test.go' 2>/dev/null | sort)
fi

if [[ -d crates ]]; then
  while IFS= read -r file; do
    scan_rust_file "$file"
  done < <(find crates -name '*.rs' -not -path '*/target/*' -not -path '*/tests/*' 2>/dev/null | sort)
fi

if (( fail )); then
  echo "scripts/check-metric-labels.sh: high-cardinality metric label(s) found" >&2
  exit 1
fi

echo "scripts/check-metric-labels.sh: OK"
