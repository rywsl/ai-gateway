"""Phase E e2e verification: fail_to_proxy override renders ErrorIR envelope.

E.2 — upstream closes connection before body: gateway must return 502 with a
JSON envelope whose `code` starts with `GW_`, and
gw_errors_total{kind="upstream_connect"} must increment by 1.

E.3 — upstream closes connection mid-stream: gateway must not inject a
synthetic SSE error event, and
gw_errors_total{kind="upstream_mid_stream"} must increment by 1.
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
    """Upstream RST-before-body -> 502 JSON envelope + gw_errors_total{kind=upstream_connect}+1.

    Uses tcp_reset_after_headers: fake accepts the connection, sends RST after
    receiving request headers but before sending any response bytes. Pingora
    surfaces this as a read/write error (body_started=False), which fail_to_proxy
    classifies as UpstreamConnect / GW_BACKEND_UNAVAILABLE.
    """
    pre = metrics_snapshot("gw_errors_total", metrics_url)

    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST", "path_regex": ".*/chat/completions"},
                    "response": {"kind": "tcp_reset_after_headers"},
                }
            ]
        }
    )

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


_STREAM_BODY = {
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": "hello"}],
    "stream": True,
}


@pytest.mark.skip(reason="e2e infra not available in agent worktree")
def test_upstream_mid_stream_failure_truncates(
    gateway_url: str,
    metrics_url: str,
    fake_upstream,
) -> None:
    """Upstream closes mid-stream -> no synthetic SSE error event + gw_errors_total{kind=upstream_mid_stream}+1.

    Uses tcp_reset_mid_body with after_bytes=50: fake sends partial response
    bytes then forces RST. Pingora surfaces this as a read error with
    body_started=True, which fail_to_proxy classifies as UpstreamMidStream.
    """
    pre_mid = metrics_snapshot("gw_errors_total", metrics_url)

    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST", "path_regex": ".*/chat/completions"},
                    "response": {"kind": "tcp_reset_mid_body", "after_bytes": 50},
                }
            ]
        }
    )

    chunks: list[str] = []
    with requests.post(
        f"{gateway_url.rstrip('/')}/v1/chat/completions",
        headers={"content-type": "application/json"},
        json=_STREAM_BODY,
        stream=True,
        timeout=15,
    ) as r:
        for line in r.iter_lines(decode_unicode=True):
            if line:
                chunks.append(line)

    assert chunks, "expected at least some SSE chunks before truncation"
    assert not any(c.startswith('data: {"error"') for c in chunks), (
        "gateway must not inject synthetic SSE error events on mid-stream upstream failure"
    )

    post_mid = metrics_snapshot("gw_errors_total", metrics_url)
    delta = label_delta(pre_mid, post_mid, 'kind="upstream_mid_stream"')
    assert delta >= 1, (
        f"expected gw_errors_total{{kind=upstream_mid_stream}} to increment by at least 1, got delta={delta}"
    )
