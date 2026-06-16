"""Phase-1 cnexus-protocol scenarios."""

from __future__ import annotations

import httpx

from conftest import error_code, label_delta, metrics_snapshot
from support.http_helpers import direct_vs_tunnel, post_chat


def _cnexus_node(node_id: str, provider_id: str) -> dict:
    return {
        "id": node_id,
        "ws_url": "ws://cnexus-tunnel-server:7000",
        "auth_token_enc": None,
        "enabled": True,
        "latency_ms": 0,
        "allowed_provider_ids": [provider_id],
        "allowed_vendor_key_ids": [],
        "tunnel_protocol": "cnexus",
    }


def test_tunnel_no_node_returns_hard_error(
    seed_nodes,
    seed_tunnel_key,
    gateway_client: httpx.Client,
    metrics_client: httpx.Client,
) -> None:
    seed_nodes()
    ids = seed_tunnel_key(suffix="no_node", require_tunnel=True)
    pre = metrics_snapshot("gw_tunnel_resolve_total", metrics_client)

    resp = post_chat(gateway_client, ids)

    assert resp.status_code == 502
    assert error_code(resp) == "GW_TUNNEL_NO_NODE"
    post = metrics_snapshot("gw_tunnel_resolve_total", metrics_client)
    assert label_delta(pre, post, 'result="no_node"') >= 1


def test_tunnel_target_denied(
    seed_nodes,
    seed_tunnel_key,
    gateway_client: httpx.Client,
    metrics_client: httpx.Client,
) -> None:
    ids = seed_tunnel_key(
        suffix="target_denied",
        require_tunnel=True,
        vendor_base_url="http://example.com:9443",
    )
    seed_nodes(_cnexus_node("denied-node", ids["vendor_id"]))
    pre = metrics_snapshot("gw_tunnel_resolve_total", metrics_client)

    resp = post_chat(gateway_client, ids)

    assert resp.status_code == 502
    assert error_code(resp) == "GW_TUNNEL_TARGET_DENIED"
    post = metrics_snapshot("gw_tunnel_resolve_total", metrics_client)
    assert label_delta(pre, post, 'result="denied"') >= 1


def test_tunnel_direct_vs_tunnel_byte_identity(
    seed_nodes,
    seed_tunnel_key,
    fake_upstream,
    gateway_client: httpx.Client,
) -> None:
    direct = seed_tunnel_key(suffix="direct", require_tunnel=False)
    tunnel = seed_tunnel_key(suffix="tunnel", require_tunnel=True)
    seed_nodes(_cnexus_node("allowed-node", tunnel["vendor_id"]))
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST", "path_regex": "^/v1/chat/completions$"},
                    "response": {
                        "kind": "sse",
                        "delay_ms": 0,
                        "events": [
                            {"data": '{"choices":[{"delta":{"content":"hi"}}]}'},
                            {"data": "[DONE]"},
                        ],
                    },
                }
            ]
        }
    )

    pair = direct_vs_tunnel(gateway_client, direct, tunnel)

    assert pair.direct.status_code == 200, pair.direct.text
    assert pair.tunnel.status_code == 200, pair.tunnel.text
    assert pair.status_match(), (
        f"status mismatch: direct={pair.direct.status_code} "
        f"tunnel={pair.tunnel.status_code}"
    )
    assert pair.bodies_match(), (
        f"body mismatch: direct={pair.direct.content[:200]!r} "
        f"tunnel={pair.tunnel.content[:200]!r}"
    )
