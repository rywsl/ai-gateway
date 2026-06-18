#!/usr/bin/env bash
# WS-12 T-07b: compare Go/Rust rustpact protocol floors.
set -euo pipefail
cd "$(dirname "$0")/.." || exit 2

if [[ "${WS12_SKIP_DEPLOY_ORDER:-0}" == "1" ]]; then
  echo "[check-deploy-order] skipped (WS12_SKIP_DEPLOY_ORDER=1)"
  exit 0
fi

local_file="crates/gw-runtime/src/contracts_version.rs"
local_proto="$(awk -F= '/RUSTPACT_PROTOCOL_VERSION[[:space:]]*:[[:space:]]*u32[[:space:]]*=/ { rhs=$2; gsub(/[^0-9]/, "", rhs); print rhs; exit }' "$local_file")"
local_min="$(awk -F= '/RUSTPACT_MIN_SUPPORTED_VERSION[[:space:]]*:[[:space:]]*u32[[:space:]]*=/ { rhs=$2; gsub(/[^0-9]/, "", rhs); print rhs; exit }' "$local_file")"

remote_file="${WS12_GO_VERSION_FILE:-../AILinkMax-Go/shared/rustpact/version.go}"
remote_raw=""
if [[ -f "$remote_file" ]]; then
  remote_raw="$(cat "$remote_file")"
else
  remote_url="${WS12_GO_VERSION_URL:-https://gitlab.ailinkmax.com/ailinkmax/CNexus-Go/-/raw/main/shared/rustpact/version.go}"
  [[ -n "$remote_url" ]] || { echo "FATAL: Go version file not found and WS12_GO_VERSION_URL unset: $remote_file" >&2; exit 2; }
  remote_raw="$(curl -fsSL --max-time 15 "$remote_url")"
fi

remote_proto="$(awk -F= '/RustpactProtocolVersion[[:space:]]*=/ { rhs=$2; gsub(/[^0-9]/, "", rhs); print rhs; exit }' <<<"$remote_raw")"
remote_min="$(awk -F= '/RustpactMinSupportedVersion[[:space:]]*=/ { rhs=$2; gsub(/[^0-9]/, "", rhs); print rhs; exit }' <<<"$remote_raw")"

if [[ -z "$local_proto" || -z "$local_min" || -z "$remote_proto" || -z "$remote_min" ]]; then
  echo "FATAL: could not parse rustpact version constants" >&2
  exit 2
fi

echo "[check-deploy-order] Rust: proto=$local_proto min=$local_min"
echo "[check-deploy-order] Go: proto=$remote_proto min=$remote_min"

fail=0
if (( remote_min < local_proto )); then
  echo "VIOLATION: Go RustpactMinSupportedVersion ($remote_min) < Rust protocol ($local_proto)" >&2
  fail=1
fi
if (( local_min > remote_proto )); then
  echo "VIOLATION: Rust RUSTPACT_MIN_SUPPORTED_VERSION ($local_min) > Go protocol ($remote_proto)" >&2
  fail=1
fi
(( fail == 0 )) || exit 1
echo "[check-deploy-order] OK"
