"""Phase E e2e verification: fail_to_proxy override renders ErrorIR envelope.

E.2 — upstream closes connection before body: gateway must return 502 with a
JSON envelope whose `code` starts with `GW_`, and
gw_errors_total{kind="upstream_connect"} must increment by 1.
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
def test_upstream_connect_failure_returns_json_envelope(
    gateway_url: str,
    metrics_url: str,
    fake_upstream,
) -> None:
    """Upstream refuse-before-body → 502 JSON envelope + gw_errors_total{kind=upstream_connect}+1."""
    pre = metrics_snapshot("gw_errors_total", metrics_url)

    fake_upstream.set_mode("refuse")

    resp = requests.post(
        f"{gateway_url.rstrip('/')}/v1/chat/completions",
        headers={"content-type": "application/json"},
        json=_CHAT_BODY,
        timeout=10,
    )

    assert resp.status_code == 502, resp.text
    body = resp.json()
    assert body.get("code", "").startswith("GW_"), (
        f"expected gateway error code starting with GW_, got: {body}"
    )

    post = metrics_snapshot("gw_errors_total", metrics_url)
    delta = label_delta(pre, post, 'kind="upstream_connect"')
    assert delta >= 1, (
        f"expected gw_errors_total{{kind=upstream_connect}} to increment by at least 1, got delta={delta}"
    )
