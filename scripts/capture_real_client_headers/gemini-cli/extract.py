"""mitmproxy addon: extract Gemini CLI static request headers.

Run via:  mitmdump -nr <flow> -s extract.py

Filters flows to the Gemini streamGenerateContent endpoint and prints the
static header subset (the same keys tracked in expected.json) as a single
JSON object to stdout. Per-key / dynamic headers (Authorization, quota
project id, request ids) are intentionally excluded so the diff only trips
on genuine drift of the hardcoded profile values.
"""

import json
import sys

from mitmproxy import http

# Substring matched against the request path. The Gemini CLI streams via
# /v1beta/models/<model>:streamGenerateContent (and generateContent).
ENDPOINT_SUBSTR = "generateContent"

# Lowercase header names that the profile module emits with the SAME literal
# value for every key and every request. Only Content-Type is static. The
# User-Agent (GeminiCLI-/<ver>/<model> (<os>; <arch>; unknown)) is per-key +
# per-request dynamic, x-gemini-api-privileged-user-id is per-key identity, and
# X-Goog-User-Project is a conditional per-key quota project id — all excluded.
STATIC_HEADERS = {
    "content-type",
}

_emitted = False


def request(flow: http.HTTPFlow) -> None:
    _emit(flow)


def _emit(flow: http.HTTPFlow) -> None:
    global _emitted
    if _emitted:
        return
    if ENDPOINT_SUBSTR not in flow.request.path:
        return
    out = {}
    for name, value in flow.request.headers.items():
        lname = name.lower()
        if lname in STATIC_HEADERS:
            out[lname] = value
    json.dump(out, sys.stdout, indent=2)
    sys.stdout.write("\n")
    _emitted = True
