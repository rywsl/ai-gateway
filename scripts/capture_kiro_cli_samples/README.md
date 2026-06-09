# Kiro CLI Sample Capture And Redaction

This directory contains an offline helper for W9 Task 1 sample intake. It helps
an operator collect real Kiro CLI traffic with mitmproxy, redact it, and produce
reviewable evidence artifacts.

This tool does not prove Kiro CLI is implemented. It only removes the sample
blocker. If captured traffic has missing usage, non-extractable usage, only
stream traffic, only non-stream traffic, or no error sample, the W9 plan stop
conditions still apply and a fallback or settlement decision is still required.

## Raw Capture Rules

Raw mitmproxy flows must stay outside the repository. Do not commit raw flow
files, raw HAR exports, screenshots with tokens, or terminal logs containing
headers.

Use a machine that already has:

- Kiro CLI installed.
- A working authenticated Kiro CLI session.
- `mitmproxy` or `mitmdump` installed.

Start capture to a path outside this repository:

```bash
mkdir -p /tmp/kiro-cli-capture
mitmproxy -p 8080 -w /tmp/kiro-cli-capture/kiro-cli.flows
```

In another terminal, run the real CLI through the proxy:

```bash
HTTPS_PROXY=http://127.0.0.1:8080 \
HTTP_PROXY=http://127.0.0.1:8080 \
  kiro <operator-selected command>
```

Capture the W9 minimum bundle from real traffic:

- Kiro CLI stream success.
- Kiro CLI non-stream success.
- Kiro CLI error response.
- OpenAI `/v1/responses` `background:true` comparison.
- OpenAI CLI comparison sample.

## Redacted Extraction

Run the extractor from the repository root:

```bash
python3 scripts/capture_kiro_cli_samples/extract.py \
  --input /tmp/kiro-cli-capture/kiro-cli.flows \
  --output-dir /tmp/kiro-cli-capture/redacted
```

The script uses the mitmproxy Python API for flow files. If mitmproxy is not
installed, it prints a clear error and also attempts a small HAR-like JSON
fallback. The script does not use the network.

The output directory contains only redacted artifacts:

- `redacted-evidence.md`: human-readable evidence with request/response header
  previews, body previews, status codes, paths, query keys, and stream
  chunk/event hints.
- `redacted-evidence.jsonl`: all matching redacted flow summaries.
- `stream.jsonl`: redacted stream-like flows.
- `non-stream.jsonl`: redacted non-stream-like flows.
- `error.jsonl`: redacted error flows.
- `comparison.jsonl`: redacted comparison flows.

If the input has no matching Kiro CLI or comparison flows, extraction exits
non-zero. Treat that as a W9 stop condition, not as a valid empty sample.

Review the generated files before copying any artifact into the repository.
Header names, paths, query keys, JSON field names, event names, status codes,
and token-count numbers are preserved. Sensitive header, query, and JSON values
are redacted by default when their names include token, cookie, signature, auth,
session, user, account, machine, device, credential, key, or related terms.

## Redaction Smoke Test

Run this before using the tool:

```bash
python3 scripts/capture_kiro_cli_samples/extract.py --self-test
```

The self-test verifies that Authorization/Bearer values, cookies, AWS
signatures, session ids, user ids, machine ids, account ids, and refresh tokens
are redacted while usage token counts and event names remain visible.

## Safety Notes

- Keep raw files outside the repo at all times.
- Do not paste raw secrets into evidence notes.
- Do not use the redacted evidence to close Kiro CLI roadmap items by itself.
- Usage absence still requires the W9 fallback or settlement decision before
  implementation closure.
