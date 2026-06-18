$ErrorActionPreference = "Stop"

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

$securityDir = "docs/security"
$investigateDir = "docs/issues/2026-06-17-investigates"

$requiredSecurityFiles = @(
  "INDEX.md",
  "disclosure-process.md",
  "cve-decision-tree.md",
  "external-reporter-sop.md",
  "changelog-template.md",
  "warn-only-protocol.md",
  "ws-16-launch-gate.md",
  "advisories/_template.md",
  "internal-audit/queries.sql"
)

foreach ($rel in $requiredSecurityFiles) {
  $path = Join-Path $securityDir $rel
  if (-not (Test-Path $path)) {
    Write-Error "missing $path"
  }
}

function Assert-Match {
  param([string]$Path, [string]$Pattern, [string]$Message)
  $text = Get-Content $Path -Raw
  if ($text -notmatch $Pattern) {
    Write-Error $Message
  }
}

for ($n = 1; $n -le 4; $n++) {
  switch ($n) {
    1 { $id = "001"; $audit = "R1"; $severity = "P0"; $cvePattern = "(pending|CVE-[0-9]{4}-[0-9]+|not-requested)" }
    2 { $id = "002"; $audit = "R2"; $severity = "P0"; $cvePattern = "(pending|CVE-[0-9]{4}-[0-9]+|not-requested)" }
    3 { $id = "003"; $audit = "R3"; $severity = "P0"; $cvePattern = "(pending|CVE-[0-9]{4}-[0-9]+|not-requested)" }
    4 { $id = "004"; $audit = "R4"; $severity = "P1"; $cvePattern = "not-requested" }
  }

  $advisory = Join-Path $securityDir "advisories/AILMX-2026-$id-$audit.md"
  $changelog = Join-Path $securityDir "advisories/AILMX-2026-$id-$audit.changelog.md"
  $csv = Join-Path $securityDir "internal-audit/$audit-affected.csv"
  $review = Join-Path $securityDir "internal-audit/$audit-affected.review.md"

  foreach ($path in @($advisory, $changelog, $csv, $review)) {
    if (-not (Test-Path $path)) { Write-Error "missing $path" }
  }

  Assert-Match $advisory "(?m)^id: AILMX-2026-$id$" "bad id in $advisory"
  Assert-Match $advisory "(?m)^audit_origin: $audit$" "bad audit_origin in $advisory"
  Assert-Match $advisory "(?m)^severity: $severity$" "bad severity in $advisory"
  Assert-Match $advisory "(?m)^status: embargoed$" "advisory must remain embargoed until external release gates pass: $advisory"
  Assert-Match $advisory "(?m)^cve_id: $cvePattern$" "bad cve_id in $advisory"
  Assert-Match $advisory "(?m)^embargo_started_utc: 2026-06-18T09:02:18Z$" "missing embargo start in $advisory"
  Assert-Match $advisory "Fix landing: 2026-06-18T09:02:18Z" "missing fix landing in $advisory"
  Assert-Match $review "BLOCKED_EXTERNAL_AUDIT" "review must declare external audit blocker: $review"

  $csvText = Get-Content $csv -Raw
  if ($csvText -match '@|sk-[A-Za-z0-9]{20}|eyJ[A-Za-z0-9_-]+\.') {
    Write-Error "potential secret or PII in $csv"
  }
}

foreach ($suffix in @("A", "B", "C", "D", "E")) {
  $path = Join-Path $investigateDir "INVESTIGATE-WS16-$suffix.md"
  if (-not (Test-Path $path)) { Write-Error "missing $path" }
  Assert-Match $path "(?m)^Status: (RESOLVED_|BLOCKED_)" "missing status in $path"
}

foreach ($id in @("001", "002", "003", "004")) {
  Assert-Match "CHANGELOG.md" "AILMX-2026-$id" "missing CHANGELOG entry for AILMX-2026-$id"
}

Assert-Match (Join-Path $securityDir "ws-16-launch-gate.md") "(?m)^Status: BLOCKED_EXTERNAL_RELEASE_GATES$" "launch gate must declare external blockers"

Write-Output "PASS: WS-16 security artifacts are locally complete and launch blockers are explicit"
