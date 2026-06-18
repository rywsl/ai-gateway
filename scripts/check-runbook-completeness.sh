#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

runbook="docs/runbooks/feature-flags.md"
if [[ ! -f "$runbook" ]]; then
  echo "ERROR: missing $runbook"
  exit 1
fi

missing=0
for flag in flags/*.yaml; do
  [[ "$(basename "$flag")" == "_schema.yaml" ]] && continue
  name="$(basename "$flag" .yaml)"
  if ! grep -q "$name" "$runbook"; then
    echo "ERROR: $runbook missing flag $name"
    missing=1
  fi
done

exit "$missing"
