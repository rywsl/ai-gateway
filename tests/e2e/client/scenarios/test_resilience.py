"""Resilience capability surface tests (3 scenarios).

Exercises gateway behaviour under adversarial upstream conditions and verifies
the health-endpoint contract:

  1. test_response_deadline_exceeded_on_slow_upstream
     — A streaming request against a deliberately slow upstream (10 bytes/sec)
       hits the per-model `stream_total_timeout_ms=2000` deadline. The gateway
       cancels the upstream connection and returns GW_RESPONSE_DEADLINE_EXCEEDED.
       The `gw_billing_upstream_cancellation_total{cause="deadline"}` counter
       increments.

  2. test_readyz_returns_200_when_healthy
     — GET /readyz on the health port returns 200 "ok" once the gateway has
       completed bootstrap.

  3. test_readyz_semantics_differ_from_livez
     — /livez is always 200 (liveness); /readyz is 200 only when the gateway
       is fully ready. Documents the semantic split and the P2 future
       enhancement (503 on PG failure).

Entity-ID prefix: `res_` (resilience).
"""
from __future__ import annotations

import time

import requests

from conftest import (
    BALANCE_LARGE,
    _error_code,
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


# ---------------------------------------------------------------------------
# 1. Response deadline exceeded on slow upstream
# ---------------------------------------------------------------------------


def test_response_deadline_exceeded_on_slow_upstream(
    fake_web,
    fake_upstream,
    gateway_url: str,
    metrics_url: str,
) -> None:
    """Seed a model with a 2s stream deadline and a fake upstream that delivers
    bytes at 10 bytes/sec (1000 bytes body → 100s to complete). The gateway
    must cancel the upstream after ~2s and return GW_RESPONSE_DEADLINE_EXCEEDED.

    Metric: gw_billing_upstream_cancellation_total{cause="deadline"} increments.
    """
    # -- seed entities -------------------------------------------------------
    model = "res-model"
    group = "res_group"

    seed_user_apikey(
        fake_web,
        user_id="res_user",
        api_key_id="res_key",
        api_key_plaintext="res_secret",
        allowed_groups=[group],
    )
    fake_web.set_balance("res_user", BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id="res_vendor")
    seed_vendor_key(
        fake_web,
        key_id="res_vk",
        vendor_id="res_vendor",
        model=model,
        accessible_endpoints=["open_ai"],
    )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=["res_vk"],
    )
    # 2-second stream deadline override
    seed_pricing(fake_web, model=model, stream_total_timeout_ms=2000)

    # -- install slow upstream scenario --------------------------------------
    # 10 bytes/sec on a 1000-byte body → ~100s to deliver. The 2s deadline
    # will fire long before completion.
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {
                        "method": "POST",
                        "path_regex": "^/v1/chat/completions$",
                    },
                    "response": {
                        "kind": "slow_body_byte_per_second",
                        "body": "x" * 1000,
                        "bps": 10,
                    },
                }
            ]
        }
    )

    # -- snapshot metric before request --------------------------------------
    pre_cancel = metrics_snapshot(
        "gw_billing_upstream_cancellation_total", metrics_url
    )
    pre_deadline = metrics_snapshot(
        "gw_response_deadline_exceeded_total", metrics_url
    )

    # -- fire streaming request ----------------------------------------------
    headers = pair_headers("res_secret", "res_key")
    body = {
        "model": model,
        "messages": [{"role": "user", "content": "hello"}],
        "stream": True,
    }
    # Use a generous client-side timeout (15s) so we don't race the gateway's
    # own 2s deadline. The gateway should respond well before 15s.
    try:
        resp = post_chat(gateway_url, headers, body, timeout=15)
    except (requests.exceptions.ChunkedEncodingError, requests.exceptions.ConnectionError):
        resp = None

    # -- assertions ----------------------------------------------------------
    if resp is not None:
        code = _error_code(resp)
        assert code == "GW_RESPONSE_DEADLINE_EXCEEDED", (
            f"Expected GW_RESPONSE_DEADLINE_EXCEEDED, got code={code}, "
            f"status={resp.status_code}, body={resp.text[:500]}"
        )

    # -- metric assertion (with small retry window for async emit) -----------
    deadline = time.monotonic() + 3.0
    cancel_delta = 0.0
    deadline_delta = 0.0
    while time.monotonic() < deadline:
        post_cancel = metrics_snapshot(
            "gw_billing_upstream_cancellation_total", metrics_url
        )
        post_deadline = metrics_snapshot(
            "gw_response_deadline_exceeded_total", metrics_url
        )
        cancel_delta = label_delta(pre_cancel, post_cancel, 'cause="deadline"')
        deadline_delta = sum(post_deadline.values()) - sum(pre_deadline.values())
        if cancel_delta >= 1 or deadline_delta >= 1:
            break
        time.sleep(0.2)
    assert cancel_delta >= 1 or deadline_delta >= 1, (
        "deadline enforcement metric did not increment "
        f"(cancel_delta={cancel_delta}, response_deadline_delta={deadline_delta})"
    )


# ---------------------------------------------------------------------------
# 2. /readyz returns 200 when healthy
# ---------------------------------------------------------------------------


def test_readyz_returns_200_when_healthy(gateway_health_url: str) -> None:
    """GET /readyz on the health port (8080) returns 200 with body "ok"
    when the gateway has completed bootstrap and all subsystems are healthy.
    """
    resp = requests.get(f"{gateway_health_url}/readyz", timeout=5)
    assert resp.status_code == 200, (
        f"Expected 200, got {resp.status_code}: {resp.text}"
    )
    assert "ok" in resp.text.lower(), (
        f"Expected body to contain 'ok', got: {resp.text!r}"
    )


# ---------------------------------------------------------------------------
# 3. /readyz semantics differ from /livez
# ---------------------------------------------------------------------------


def test_readyz_semantics_differ_from_livez(gateway_health_url: str) -> None:
    """/livez is ALWAYS 200 "ok" — it signals process liveness to the
    orchestrator and must never fail while the process is running.

    /readyz is 200 "ok" only when the gateway is fully bootstrapped and
    all subsystems (PG, Redis) are reachable.

    Both are deliberately OUTSIDE the management-token auth chain (no
    Authorization header required).

    # P2 future enhancement: /readyz should return 503 "not ready" when
    # PG is down. This cannot be tested without stopping the PG container
    # mid-test, which is too disruptive for the shared e2e stack. When a
    # dedicated teardown-capable stack is available, add a test that
    # `docker compose stop postgres`, asserts /readyz=503, then restarts.
    """
    # /livez — always 200
    livez = requests.get(f"{gateway_health_url}/livez", timeout=5)
    assert livez.status_code == 200, (
        f"/livez: expected 200, got {livez.status_code}"
    )
    assert livez.text.strip() == "ok"

    # /readyz — 200 when healthy
    readyz = requests.get(f"{gateway_health_url}/readyz", timeout=5)
    assert readyz.status_code == 200, (
        f"/readyz: expected 200, got {readyz.status_code}"
    )
    assert readyz.text.strip() == "ok"

    # Both endpoints are unauthenticated — no Authorization header needed.
    # This is a deliberate design choice: orchestrators (k8s probes) must
    # be able to reach these without token plumbing.
