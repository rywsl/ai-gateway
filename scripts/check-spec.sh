#!/usr/bin/env bash
# Lints one as-built domain spec for structure + no-aspirational-language + anchored claims.
set -euo pipefail
f="${1:?usage: check-spec.sh docs/spec/0N-name.md}"
fail=0
need() { grep -qE "^## $1" "$f" || { echo "[$f] MISSING section: ## $1"; fail=1; }; }
need "Purpose"
need "As-built surface"
need "Not built"
# As-built specs must not contain aspirational / status language.
if grep -nE 'Phase [0-9]|will be added|unimplemented!|todo!|TODO\(v2\)|integrated by|🚧|✅|\blanded\b' "$f"; then
  echo "[$f] aspirational/status language found (as-built specs describe only what exists)"; fail=1
fi
# The As-built surface must anchor claims to real source files.
awk '/^## As-built surface/{a=1;next}/^## /{a=0}a' "$f" | grep -qE '\.rs' \
  || { echo "[$f] As-built surface has no .rs anchors"; fail=1; }
[ "$fail" = 0 ] && echo "[$f] ok"
exit "$fail"
