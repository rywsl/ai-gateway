"""mitmproxy addon: extract Claude Code static request headers.

Run via:  mitmdump -nr <flow> -s extract.py

Filters flows to the Anthropic Messages endpoint and prints the static
header subset (the same keys tracked in expected.json) as a single JSON
object to stdout. Per-key / dynamic headers (Authorization, request ids,
session/machine identity) are intentionally excluded so the diff only
trips on genuine drift of the hardcoded profile values.
"""

import json
import sys

from mitmproxy import http

# Endpoint the Claude Code CLI hits.
ENDPOINT_SUFFIX = "/v1/messages"

# Lowercase header names that the profile module emits with the SAME literal
# value for every key and every request. The User-Agent and every
# X-Stainless-{OS,Arch,Runtime-Version,Package-Version,Timeout} header are
# per-key dynamic (derived from the seeded identity pools) and are excluded.
# x-stainless-helper-method is per-request (stream only) and is excluded too.
STATIC_HEADERS = {
    "accept",
    "anthropic-version",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
    "x-app",
    "x-stainless-lang",
    "x-stainless-runtime",
    "x-stainless-retry-count",
}

_emitted = False


def response(flow: http.HTTPFlow) -> None:
    _emit(flow)


def request(flow: http.HTTPFlow) -> None:
    _emit(flow)


def _emit(flow: http.HTTPFlow) -> None:
    global _emitted
    if _emitted:
        return
    if not flow.request.path.startswith(ENDPOINT_SUFFIX):
        return
    out = {}
    for name, value in flow.request.headers.items():
        lname = name.lower()
        if lname in STATIC_HEADERS:
            out[lname] = value
    json.dump(out, sys.stdout, indent=2)
    sys.stdout.write("\n")
    _emitted = True
