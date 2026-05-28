"""mitmproxy addon: extract OpenAI Codex static request headers.

Run via:  mitmdump -nr <flow> -s extract.py

Filters flows to the Codex Responses endpoint and prints the static
header subset (the same keys tracked in expected.json) as a single JSON
object to stdout. Per-key / dynamic headers (Authorization, request ids,
session identity) are intentionally excluded so the diff only trips on
genuine drift of the hardcoded profile values.
"""

import json
import sys

from mitmproxy import http

# Endpoint the Codex CLI hits.
ENDPOINT_SUFFIX = "/v1/responses"

# Lowercase header names that the profile module emits with the SAME literal
# value for every key and every request. Only OpenAI-Beta is truly static.
# Accept is per-request (text/event-stream vs application/json), User-Agent is
# per-key dynamic (codex_cli_rs/<version> (<os>; <arch>)), and the
# x-codex-installation-id / session-id / thread-id / x-client-request-id /
# x-codex-window-id / x-codex-turn-state headers are per-key or per-request
# identity/turn-state — all excluded.
STATIC_HEADERS = {
    "openai-beta",
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
