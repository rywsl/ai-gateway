#!/usr/bin/env bash
# WS-12 T-09: continuous-discovery audit. Warn-only by design.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 2

base="${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$(git merge-base origin/main HEAD 2>/dev/null || true)}"
if [[ -z "$base" ]]; then
  echo "[audit-newfindings] no merge base; skipping"
  exit 0
fi

files=()
while IFS= read -r file; do
  files+=("$file")
done < <(git diff --name-only --diff-filter=AM "$base"...HEAD | grep -vE '/testdata/|_test\.go$|/tests/|\.md$|/migrations/|target/' || true)
if (( ${#files[@]} == 0 )); then
  echo "[audit-newfindings] OK - no changed source files"
  exit 0
fi

report="$(mktemp "${TMPDIR:-/tmp}/ws12-rust-newfindings.XXXXXX")"
found=0
{
  echo "# WS-12 audit-newfindings auto report"
  echo
  echo "- Base: \`$base\`"
  echo "- Files changed: ${#files[@]}"
  echo

  titles=(
    "P1: Rust silent let _ ="
    "P3: Rust unwrap/expect"
    "P4: sleep"
    "P5: deferred TODO"
    "P6: panic in production path"
  )
  patterns=(
    '^[[:space:]]*let[[:space:]]+_[[:space:]]*='
    '\.unwrap\(\)|\.expect\('
    'tokio::time::sleep'
    '(TODO|FIXME|XXX).*(later|next sprint|tomorrow)'
    '\bpanic!|\bpanic\('
  )
  suffixes=(
    '\.rs:'
    '\.rs:'
    ':'
    ':'
    ':'
  )
  for idx in "${!titles[@]}"; do
    title="${titles[$idx]}"
    pattern="${patterns[$idx]}"
    suffix="${suffixes[$idx]}"
    hits="$(grep -nE "$pattern" "${files[@]}" 2>/dev/null | grep -E "$suffix" | grep -vE '_test\.|/tests/' || true)"
    if [[ -n "$hits" ]]; then
      found=1
      echo "## $title"
      echo '```'
      echo "$hits" | head -80
      echo '```'
      echo
    fi
  done
} > "$report"

if (( found )); then
  if [[ "${WS12_AUDIT_WRITE:-0}" == "1" || -n "${CI:-}" ]]; then
    branch="$(git rev-parse --abbrev-ref HEAD | tr '/ ' '__')"
    sha="$(git rev-parse --short HEAD)"
    out="docs/issues/2026-06-17-investigates/auto-${branch}-${sha}.md"
    cp "$report" "$out"
    echo "[audit-newfindings] WARN - report written to $out"
  else
    echo "[audit-newfindings] WARN - suspicious patterns found (set WS12_AUDIT_WRITE=1 to persist report)"
    sed -n '1,120p' "$report"
  fi
else
  echo "[audit-newfindings] OK - no suspicious patterns"
fi
