#!/bin/bash
# scripts/capture_real_client_headers/codex/capture.sh
#
# Diff the static request headers the *real* OpenAI Codex CLI sends against
# the baseline in expected.json (which mirrors the static_headers() in
# crates/gw-core/src/impersonate/openai_codex.rs).
#
# Operator workflow (see ../README.md for the full local recipe):
#   1. Start mitmproxy:  mitmproxy -p 8080 -w /tmp/flow
#   2. Run the real CLI through it under OAuth:
#        HTTPS_PROXY=http://localhost:8080 codex ...        # any real turn
#   3. Point this script at the captured flow file:
#        MITMPROXY_FLOW_FILE=/tmp/flow ./capture.sh
#
# CLI install + OAuth are NOT performed here: the Codex CLI requires an
# interactive authenticated session, so the capture step is operator-driven.
# This script only handles extraction + drift comparison.
set -euo pipefail

PROFILE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXPECTED="$PROFILE_DIR/expected.json"
OBSERVED="$(mktemp -d)/observed.json"

: "${MITMPROXY_FLOW_FILE:?Set MITMPROXY_FLOW_FILE to the captured mitmproxy flow file}"

# Extract the static-header subset for /v1/responses from the mitmproxy flow.
mitmdump -nr "$MITMPROXY_FLOW_FILE" \
  -s "$PROFILE_DIR/extract.py" > "$OBSERVED"

if ! diff <(jq -S . "$EXPECTED") <(jq -S . "$OBSERVED"); then
  echo "DRIFT in codex headers"
  echo "  expected: $EXPECTED"
  echo "  observed: $OBSERVED"
  exit 1
fi
echo "codex headers match expected baseline"
