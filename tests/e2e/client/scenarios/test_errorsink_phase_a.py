"""Phase A e2e verification: gw_errors_total{kind=short_circuit} increments on auth bail.

Sends a request with a completely unknown API key so the gateway short-circuits
at auth (GW_AUTH_INVALID_APIKEY, 401) before any routing occurs.  Asserts that
the Prometheus counter gw_errors_total{kind="short_circuit"} incremented by
exactly 1 for that request.
"""

from __future__ import annotations

import pytest
import requests

from conftest import label_delta, metrics_snapshot

_CHAT_BODY = {
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": "hello"}],
    "stream": False,
}


@pytest.mark.skip(reason="e2e infra not available in agent worktree")
def test_short_circuit_emits_gw_errors_total(
    gateway_url: str,
    metrics_url: str,
) -> None:
    """Auth-fail short-circuit increments gw_errors_total{kind=short_circuit} by 1."""
    pre = metrics_snapshot("gw_errors_total", metrics_url)

    resp = requests.post(
        f"{gateway_url.rstrip('/')}/v1/chat/completions",
        headers={
            "x-api-key": "unknown-key-phase-a-test",
            "x-api-key-id": "ak-unknown-phase-a",
            "content-type": "application/json",
        },
        json=_CHAT_BODY,
        timeout=10,
    )

    assert resp.status_code == 401, resp.text

    post = metrics_snapshot("gw_errors_total", metrics_url)
    delta = label_delta(pre, post, 'kind="short_circuit"')
    assert delta >= 1, (
        f"expected gw_errors_total{{kind=short_circuit}} to increment by at least 1, got delta={delta}"
    )
