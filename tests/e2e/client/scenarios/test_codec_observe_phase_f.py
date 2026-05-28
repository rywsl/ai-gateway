"""Phase F e2e verification: malformed SSE chunk increments gw_errors_total{kind=codec_parse}.

The fake upstream is instructed to emit a malformed SSE data line so the codec
parse path calls observe(CodecParse).  The Prometheus counter
gw_errors_total{kind="codec_parse"} must increment by at least 1.
"""

from __future__ import annotations

import pytest
import requests

from conftest import label_delta, metrics_snapshot

_CHAT_BODY = {
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": "hello"}],
    "stream": True,
}


@pytest.mark.skip(reason="e2e infra not available in agent worktree")
def test_malformed_sse_emits_codec_parse_metric(
    gateway_url: str,
    metrics_url: str,
    fake_upstream_url: str,
) -> None:
    """Malformed SSE payload from upstream increments gw_errors_total{kind=codec_parse}."""
    # Tell the fake upstream to inject a malformed SSE data frame.
    requests.post(
        f"{fake_upstream_url.rstrip('/')}/control/mode",
        json={"mode": "malformed_sse"},
        timeout=5,
    ).raise_for_status()

    pre = metrics_snapshot("gw_errors_total", metrics_url)

    with requests.post(
        f"{gateway_url.rstrip('/')}/v1/chat/completions",
        headers={"content-type": "application/json"},
        json=_CHAT_BODY,
        stream=True,
        timeout=15,
    ) as resp:
        # Drain all SSE lines; the gateway must not hard-close the stream.
        list(resp.iter_lines())

    post = metrics_snapshot("gw_errors_total", metrics_url)
    delta = label_delta(pre, post, 'kind="codec_parse"')
    assert delta >= 1, (
        f"expected gw_errors_total{{kind=codec_parse}} to increment by at least 1, got delta={delta}"
    )

    # Reset fake upstream to normal mode so other tests are not affected.
    requests.post(
        f"{fake_upstream_url.rstrip('/')}/control/mode",
        json={"mode": "normal"},
        timeout=5,
    ).raise_for_status()
