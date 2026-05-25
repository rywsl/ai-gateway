"""Tunnel-mode e2e scenarios.

The current docker e2e gateway image is built by the repository root
``Dockerfile`` with:

    cargo build --release --locked -p gw-gateway

and ``tests/e2e/docker-compose.yml`` does not override that with
``--features tunnel`` or start a tunnel-node service. Keep these scenarios in
the suite as executable documentation, but skip them until the e2e Docker infra
builds the gateway with tunnel support and supplies a live tunnel node.
"""

from __future__ import annotations

import pytest

from conftest import (
    BALANCE_LARGE,
    PROTO_OPENAI,
    _error_code,
    encrypt_upstream_key,
    label_delta,
    make_openai_client,
    metrics_snapshot,
    pair_headers,
    post_chat,
    seed_model_group,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
)


pytestmark = pytest.mark.skip(
    reason=(
        "requires --features tunnel build — see "
        "docs/superpowers/specs/2026-05-22-tunnel-t-design.md"
    )
)


def _seed_tunnel_key(fake_web, *, suffix: str, require_tunnel: bool = True) -> dict:
    ids = {
        "user_id": f"tn_user_{suffix}",
        "api_key_id": f"tn_ak_{suffix}",
        "api_key_plaintext": f"tn_sk_client_{suffix}",
        "vendor_id": f"tn_vendor_{suffix}",
        "key_id": f"tn_vk_{suffix}",
        "group": f"tn_group_{suffix}",
        "model": f"tn_model_{suffix}",
    }
    seed_user_apikey(
        fake_web,
        user_id=ids["user_id"],
        api_key_id=ids["api_key_id"],
        api_key_plaintext=ids["api_key_plaintext"],
        allowed_groups=[ids["group"]],
    )
    seed_vendor(fake_web, vendor_id=ids["vendor_id"])
    seed_vendor_key(
        fake_web,
        key_id=ids["key_id"],
        vendor_id=ids["vendor_id"],
        model=ids["model"],
        accessible_endpoints=[PROTO_OPENAI],
    )
    fake_web.put_vendor_key(
        ids["key_id"],
        key_id=ids["key_id"],
        vendor_id=ids["vendor_id"],
        api_key_enc=encrypt_upstream_key("sk-up"),
        accessible_models=[ids["model"]],
        model_mapping={},
        accessible_endpoints=[PROTO_OPENAI],
        protocol_conversion=False,
        priority=0,
        weight=1,
        rpm_limit=0,
        tpm_limit=0,
        max_usage_count=None,
        proxy=None,
        ip_allowlist=None,
        expires_at=None,
        status="active",
        tags=[],
        forced_capabilities=[],
        exclusive_capability_match=False,
        tunnel_preference={"require_tunnel": require_tunnel},
    )
    seed_model_group(fake_web, group=ids["group"], vendor_key_ids=[ids["key_id"]])
    seed_pricing(fake_web, model=ids["model"])
    fake_web.set_balance(ids["user_id"], BALANCE_LARGE)
    return ids


def test_tunnel_no_node_returns_hard_error(gateway_url, fake_web, metrics_url) -> None:
    ids = _seed_tunnel_key(fake_web, suffix="no_node", require_tunnel=True)
    pre = metrics_snapshot("gw_tunnel_resolve_total", metrics_url)

    resp = post_chat(
        gateway_url,
        pair_headers(ids["api_key_plaintext"], ids["api_key_id"]),
        {"model": ids["model"], "messages": [{"role": "user", "content": "hi"}]},
    )

    assert resp.status_code == 502
    assert _error_code(resp) == "GW_TUNNEL_NO_NODE"
    post = metrics_snapshot("gw_tunnel_resolve_total", metrics_url)
    assert label_delta(pre, post, 'result="no_node"') >= 1


def test_tunnel_target_denied(gateway_url, fake_web, metrics_url) -> None:
    ids = _seed_tunnel_key(fake_web, suffix="target_denied", require_tunnel=True)
    pre = metrics_snapshot("gw_tunnel_resolve_total", metrics_url)

    resp = post_chat(
        gateway_url,
        pair_headers(ids["api_key_plaintext"], ids["api_key_id"]),
        {"model": ids["model"], "messages": [{"role": "user", "content": "hi"}]},
    )

    assert resp.status_code == 502
    assert _error_code(resp) == "GW_TUNNEL_TARGET_DENIED"
    post = metrics_snapshot("gw_tunnel_resolve_total", metrics_url)
    assert label_delta(pre, post, 'result="denied"') >= 1


def test_tunnel_direct_vs_tunnel_byte_identity(gateway_url, fake_web, fake_upstream) -> None:
    direct = _seed_tunnel_key(fake_web, suffix="direct", require_tunnel=False)
    tunnel = _seed_tunnel_key(fake_web, suffix="tunnel", require_tunnel=True)
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST"},
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

    def _content(ids: dict) -> str:
        client = make_openai_client(
            gateway_url, ids["api_key_plaintext"], ids["api_key_id"]
        )
        stream = client.chat.completions.create(
            model=ids["model"],
            messages=[{"role": "user", "content": "hi"}],
            stream=True,
        )
        return "".join(
            chunk.choices[0].delta.content
            for chunk in stream
            if chunk.choices and chunk.choices[0].delta.content
        )

    assert _content(direct) == _content(tunnel) == "hi"
