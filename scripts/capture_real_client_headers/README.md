# Real-client header capture + drift detection

Operational tooling that verifies our impersonation profiles still match the
**static** request headers the real upstream CLIs send. If Anthropic / OpenAI /
Google bump a `User-Agent`, beta flag, or SDK tag, this catches the drift so we
can refresh the profile modules in
`crates/gw-core/src/impersonate/<profile>.rs`.

## Layout

```
<profile>/
  expected.json   # baseline: { "header-name-lowercase": "literal-value" }
  extract.py      # mitmproxy addon: emits the static-header subset as JSON
  capture.sh      # runs extract.py over a flow file and diffs vs expected.json
```

Profiles: `claude-code`, `codex`, `gemini-cli`.

`expected.json` holds only the **static** headers — values that are identical
for every request and every key. Per-key / dynamic headers (Authorization,
request ids, session / machine identity, quota project id) are excluded; they
are intentionally not part of the drift baseline.

## Endpoints filtered

- claude-code -> `/v1/messages`
- codex -> `/v1/responses`
- gemini-cli -> `*streamGenerateContent*` (matches `generateContent`)

## Refreshing expected.json

When a profile module changes in `crates/gw-core/src/impersonate/`, update the
matching `expected.json` so the keys/values mirror that module's
`static_headers()` verbatim. Keys must be lowercase (mitmproxy normalizes on
read); values are the exact strings the profile emits.

## Running locally

```bash
# terminal 1: start mitmproxy, write flows to /tmp/flow
mitmproxy -p 8080 -w /tmp/flow

# terminal 2: run the real CLI through the proxy (real OAuth session required)
HTTPS_PROXY=http://localhost:8080 claude --help

# terminal 2: diff captured headers against the baseline
MITMPROXY_FLOW_FILE=/tmp/flow ./scripts/capture_real_client_headers/claude-code/capture.sh
```

A clean run prints `<profile> headers match expected baseline`. On drift the
script prints `DRIFT in <profile> headers`, shows the `diff`, and exits 1.

## OAuth caveat

The CLIs authenticate via interactive OAuth, so CI cannot run end-to-end
unattended. The `header-drift` GitHub workflow installs mitmproxy and wires up
the diff step, but the CLI install + authenticated capture must be performed by
an operator (manual `workflow_dispatch` in an authenticated environment, or a
local run as above).
