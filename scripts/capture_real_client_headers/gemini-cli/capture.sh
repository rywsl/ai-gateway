#!/bin/bash
# scripts/capture_real_client_headers/gemini-cli/capture.sh
#
# Diff the static request headers the *real* Gemini CLI sends against the
# baseline in expected.json (which mirrors the static_headers() in
# crates/gw-core/src/impersonate/gemini_cli.rs).
#
# Operator workflow (see ../README.md for the full local recipe):
#   1. Start mitmproxy:  mitmproxy -p 8080 -w /tmp/flow
#   2. Run the real CLI through it under OAuth:
#        HTTPS_PROXY=http://localhost:8080 gemini ...       # any real turn
#   3. Point this script at the captured flow file:
#        MITMPROXY_FLOW_FILE=/tmp/flow ./capture.sh
#
# CLI install + OAuth are NOT performed here: the Gemini CLI requires an
# interactive authenticated session, so the capture step is operator-driven.
# This script only handles extraction + drift comparison.
set -euo pipefail

PROFILE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXPECTED="$PROFILE_DIR/expected.json"
OBSERVED="$(mktemp)"

if [[ -z "${MITMPROXY_FLOW_FILE:-}" ]]; then
  echo "MITMPROXY_FLOW_FILE not set — skipping gemini-cli drift check (unattended run cannot capture authenticated traffic)."
  echo "To run: capture a flow with mitmproxy, then re-run via workflow_dispatch or locally with MITMPROXY_FLOW_FILE set. See scripts/capture_real_client_headers/README.md."
  exit 0
fi
if [[ ! -f "$MITMPROXY_FLOW_FILE" ]]; then
  echo "ERROR: MITMPROXY_FLOW_FILE=$MITMPROXY_FLOW_FILE does not exist."
  exit 1
fi

# Extract the static-header subset for streamGenerateContent from the flow.
mitmdump -nr "$MITMPROXY_FLOW_FILE" \
  -s "$PROFILE_DIR/extract.py" > "$OBSERVED"

if [[ ! -s "$OBSERVED" ]]; then
  echo "ERROR: no matching flows found in $MITMPROXY_FLOW_FILE for this profile."
  exit 1
fi

if ! diff <(jq -S . "$EXPECTED") <(jq -S . "$OBSERVED"); then
  echo "DRIFT in gemini-cli headers"
  echo "  expected: $EXPECTED"
  echo "  observed: $OBSERVED"
  exit 1
fi
echo "gemini-cli headers match expected baseline"
