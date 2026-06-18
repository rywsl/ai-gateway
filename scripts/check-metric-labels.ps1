$ErrorActionPreference = "Stop"

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

# M13 canonical banned set. Do not add bounded enum labels such as
# result, op, path, attempt_index, or model_id.
$banned = @("request_id", "api_key_id", "user_id", "ip", "ip_address", "email", "token", "session_id", "jti", "refresh_token", "access_token", "hash")
$allowPattern = '//\s*(allow-label|metric-label):\s+\S+\s+reason="[^"]{10,}"'
$failed = $false

function Test-AllowNear {
  param([string[]]$Lines, [int]$Index)
  $start = [Math]::Max(0, $Index - 3)
  $end = $Index
  $block = ($Lines[$start..$end] -join "`n")
  return $block -match $allowPattern
}

function Scan-GoFile {
  param([string]$File)
  $lines = Get-Content $File
  for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -notmatch 'New(Counter|Gauge|Histogram|Summary)Vec\b') { continue }
    $start = $i
    $end = [Math]::Min($lines.Count - 1, $i + 12)
    $windowLines = @()
    for ($j = $start; $j -le $end; $j++) {
      $windowLines += $lines[$j]
      if ($lines[$j] -match '(\)|^|\s)\.(increment|record|set)\(') { break }
    }
    $window = ($windowLines -join "`n")
    foreach ($label in $banned) {
      if ($window -match [Regex]::Escape("`"$label`"")) {
        if (Test-AllowNear -Lines $lines -Index $i) { continue }
        Write-Output "VIOLATION: ${File}:$($i + 1) metric uses banned label '$label'"
        $script:failed = $true
      }
    }
  }
}

function Scan-RustFile {
  param([string]$File)
  $lines = Get-Content $File
  for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -notmatch 'metrics::(counter|gauge|histogram)!|register_(int_counter|counter|gauge|histogram|int_gauge)_vec!') { continue }
    $start = $i
    $end = [Math]::Min($lines.Count - 1, $i + 12)
    $windowLines = @()
    for ($j = $start; $j -le $end; $j++) {
      $windowLines += $lines[$j]
      if ($lines[$j] -match '(\)|^|\s)\.(increment|record|set)\(') { break }
    }
    $window = ($windowLines -join "`n")
    foreach ($label in $banned) {
      if ($window -match "`"$([Regex]::Escape($label))`"\s*(=>|,)") {
        if (Test-AllowNear -Lines $lines -Index $i) { continue }
        Write-Output "VIOLATION: ${File}:$($i + 1) metric uses banned label '$label'"
        $script:failed = $true
      }
    }
  }
}

foreach ($dir in @("shared", "modules", "binaries")) {
  if (-not (Test-Path $dir)) { continue }
  Get-ChildItem $dir -Recurse -Filter *.go |
    Where-Object { $_.Name -notlike "*_test.go" } |
    ForEach-Object { Scan-GoFile $_.FullName }
}

if (Test-Path "crates") {
  Get-ChildItem "crates" -Recurse -Filter *.rs |
    Where-Object { $_.FullName -notmatch "[/\\]target[/\\]" -and $_.FullName -notmatch "[/\\]tests[/\\]" } |
    ForEach-Object { Scan-RustFile $_.FullName }
}

if ($failed) {
  Write-Error "scripts/check-metric-labels.ps1: high-cardinality metric label(s) found"
}

Write-Output "scripts/check-metric-labels.ps1: OK"
