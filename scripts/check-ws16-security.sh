#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

security_dir="docs/security"
investigate_dir="docs/issues/2026-06-17-investigates"

required_security_files=(
  "INDEX.md"
  "disclosure-process.md"
  "cve-decision-tree.md"
  "external-reporter-sop.md"
  "changelog-template.md"
  "warn-only-protocol.md"
  "ws-16-launch-gate.md"
  "advisories/_template.md"
  "internal-audit/queries.sql"
)

for rel in "${required_security_files[@]}"; do
  test -f "$security_dir/$rel" || { echo "missing $security_dir/$rel" >&2; exit 1; }
done

for n in 1 2 3 4; do
  case "$n" in
    1) id="001"; audit="R1"; severity="P0"; cve_pattern='^(pending|CVE-[0-9]{4}-[0-9]+|not-requested)$' ;;
    2) id="002"; audit="R2"; severity="P0"; cve_pattern='^(pending|CVE-[0-9]{4}-[0-9]+|not-requested)$' ;;
    3) id="003"; audit="R3"; severity="P0"; cve_pattern='^(pending|CVE-[0-9]{4}-[0-9]+|not-requested)$' ;;
    4) id="004"; audit="R4"; severity="P1"; cve_pattern='^not-requested$' ;;
  esac
  advisory="$security_dir/advisories/AILMX-2026-${id}-${audit}.md"
  changelog="$security_dir/advisories/AILMX-2026-${id}-${audit}.changelog.md"
  csv="$security_dir/internal-audit/${audit}-affected.csv"
  review="$security_dir/internal-audit/${audit}-affected.review.md"

  test -f "$advisory" || { echo "missing $advisory" >&2; exit 1; }
  test -f "$changelog" || { echo "missing $changelog" >&2; exit 1; }
  test -f "$csv" || { echo "missing $csv" >&2; exit 1; }
  test -f "$review" || { echo "missing $review" >&2; exit 1; }

  grep -q "^id: AILMX-2026-${id}$" "$advisory" || { echo "bad id in $advisory" >&2; exit 1; }
  grep -q "^audit_origin: ${audit}$" "$advisory" || { echo "bad audit_origin in $advisory" >&2; exit 1; }
  grep -q "^severity: ${severity}$" "$advisory" || { echo "bad severity in $advisory" >&2; exit 1; }
  grep -q "^status: embargoed$" "$advisory" || { echo "advisory must remain embargoed until external release gates pass: $advisory" >&2; exit 1; }
  grep -Eq "^cve_id: ${cve_pattern#^}" "$advisory" || { echo "bad cve_id in $advisory" >&2; exit 1; }
  grep -q "^embargo_started_utc: 2026-06-18T09:02:18Z$" "$advisory" || { echo "missing embargo start in $advisory" >&2; exit 1; }
  grep -q "Fix landing: 2026-06-18T09:02:18Z" "$advisory" || { echo "missing fix landing in $advisory" >&2; exit 1; }
  grep -q "BLOCKED_EXTERNAL_AUDIT" "$review" || { echo "review must declare external audit blocker: $review" >&2; exit 1; }

  if grep -Eq '@|sk-[A-Za-z0-9]{20}|eyJ[A-Za-z0-9_-]+\.' "$csv"; then
    echo "potential secret or PII in $csv" >&2
    exit 1
  fi
done

for suffix in A B C D E; do
  f="$investigate_dir/INVESTIGATE-WS16-${suffix}.md"
  test -f "$f" || { echo "missing $f" >&2; exit 1; }
  grep -Eq '^Status: (RESOLVED_|BLOCKED_)' "$f" || { echo "missing status in $f" >&2; exit 1; }
done

for id in 001 002 003 004; do
  grep -q "AILMX-2026-${id}" CHANGELOG.md || { echo "missing CHANGELOG entry for AILMX-2026-${id}" >&2; exit 1; }
done

grep -q '^Status: BLOCKED_EXTERNAL_RELEASE_GATES$' "$security_dir/ws-16-launch-gate.md" || {
  echo "launch gate must declare external blockers" >&2
  exit 1
}

echo "PASS: WS-16 security artifacts are locally complete and launch blockers are explicit"
