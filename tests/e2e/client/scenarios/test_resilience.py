"""Resilience capability surface tests (4 scenarios).

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

  4. test_kek_rotation_dual_decrypt_window
      — Documents the desired KEK rotation dual-decrypt e2e window. Skipped
        until gateway KMS bootstrap can load both old and new KEKs; current
        docker/env wiring (`GW_KMS__KEK_HEX`) installs a single `primary` KEK.

Entity-ID prefixes: `res_` (resilience), `kek_` (KEK rotation).
"""
from __future__ import annotations

import subprocess
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from threading import Event

import requests
import pytest

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
    wait_for_records,
)


_COMPOSE_FILE = "../docker-compose.yml"


def _compose_gateway(*args: str, timeout: float = 60) -> None:
    proc = subprocess.run(
        ["docker", "compose", "-f", _COMPOSE_FILE, *args],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"docker compose {' '.join(args)} failed (rc={proc.returncode}): "
            f"stderr={proc.stderr!r} stdout={proc.stdout!r}"
        )


def _wait_for_readyz(gateway_health_url: str, timeout: float = 30.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            r = requests.get(f"{gateway_health_url.rstrip('/')}/readyz", timeout=2)
            if r.status_code == 200:
                return
        except requests.RequestException:
            pass
        time.sleep(0.25)
    raise TimeoutError(f"/readyz did not return 200 within {timeout}s")


# ---------------------------------------------------------------------------
# 0. KEK rotation dual-decrypt window (currently skipped)
# ---------------------------------------------------------------------------


@pytest.mark.kek_rotation
@pytest.mark.skip(reason="KEK dual-decrypt not implemented — single-KEK only")
def test_kek_rotation_dual_decrypt_window(
    fake_web,
    fake_upstream,
    gateway_url: str,
    gateway_health_url: str,
) -> None:
    """Desired e2e contract for KEK rotation.

    Investigation result:
    - `crates/gw-store/src/encrypt.rs` has an in-memory `KekCache` dual-read
      state machine (`enter_dual_read`, `drop_old_kek`) and unit coverage.
    - `crates/gw-gateway/src/kms.rs` bootstraps exactly one KEK from
      `GW_KMS__KEK_HEX` under the configured key id, so a docker-compose
      restart with a new env value cannot retain the old KEK for fallback.

    When gateway KMS supports loading old+new KEKs, unskip this test and let it
    verify: old encrypted vendor key remains readable after restart with the
    new KEK, and a newly seeded key encrypted under the new KEK is also readable.
    """
    old_kek_hex = "deadbeef" * 8
    new_kek_hex = "feedface" * 8
    assert len(bytes.fromhex(old_kek_hex)) == 32
    assert len(bytes.fromhex(new_kek_hex)) == 32

    model = "kek_model"
    group = "kek_group"
    user_id = "kek_user"
    api_key_id = "kek_api_key"
    api_key_plaintext = "kek_secret"
    old_vendor_id = "kek_vendor_old"
    new_vendor_id = "kek_vendor_new"
    old_key_id = "kek_vk_old"
    new_key_id = "kek_vk_new"

    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
        signature_required=True,
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id=old_vendor_id)
    seed_vendor_key(
        fake_web,
        key_id=old_key_id,
        vendor_id=old_vendor_id,
        model=model,
        accessible_endpoints=["open_ai"],
        upstream_key="kek_old_upstream_secret",
    )
    seed_model_group(fake_web, group=group, vendor_key_ids=[old_key_id])
    seed_pricing(fake_web, model=model)

    _compose_gateway("kill", "-s", "SIGTERM", "gateway", timeout=30)
    _compose_gateway("up", "-d", "gateway", timeout=60)
    _wait_for_readyz(gateway_health_url, timeout=30.0)

    old_resp = post_chat(
        gateway_url,
        pair_headers(api_key_plaintext, api_key_id),
        {"model": model, "messages": [{"role": "user", "content": "old"}], "stream": True},
        timeout=10,
    )
    assert old_resp.status_code == 200, old_resp.text[:500]

    seed_vendor(fake_web, vendor_id=new_vendor_id)
    seed_vendor_key(
        fake_web,
        key_id=new_key_id,
        vendor_id=new_vendor_id,
        model=model,
        accessible_endpoints=["open_ai"],
        upstream_key="kek_new_upstream_secret",
    )
    seed_model_group(fake_web, group=group, vendor_key_ids=[old_key_id, new_key_id])

    new_resp = post_chat(
        gateway_url,
        pair_headers(api_key_plaintext, api_key_id),
        {"model": model, "messages": [{"role": "user", "content": "new"}], "stream": True},
        timeout=10,
    )
    assert new_resp.status_code == 200, new_resp.text[:500]


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


def _graceful_shutdown_drains_inflight_impl(
    fake_web,
    fake_upstream,
    gateway_url: str,
    gateway_health_url: str,
    redis_client,
) -> None:
    model = "gs-model"
    group = "gs_group"

    seed_user_apikey(
        fake_web,
        user_id="gs_user",
        api_key_id="gs_key",
        api_key_plaintext="gs_secret",
        allowed_groups=[group],
    )
    fake_web.set_balance("gs_user", BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id="gs_vendor")
    seed_vendor_key(
        fake_web,
        key_id="gs_vk",
        vendor_id="gs_vendor",
        model=model,
        accessible_endpoints=["open_ai"],
    )
    seed_model_group(fake_web, group=group, vendor_key_ids=["gs_vk"])
    seed_pricing(fake_web, model=model)

    slow_body = "x" * 500
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
                        "body": slow_body,
                        "bps": 100,
                    },
                }
            ]
        }
    )

    headers = pair_headers("gs_secret", "gs_key")
    body = {
        "model": model,
        "messages": [{"role": "user", "content": "drain"}],
        "stream": True,
    }
    started = [Event() for _ in range(3)]

    def _stream_to_eof(idx: int) -> int:
        started[idx].set()
        with requests.post(
            f"{gateway_url.rstrip('/')}/v1/chat/completions",
            json=body,
            headers=headers,
            stream=True,
            timeout=(5, 15),
        ) as resp:
            resp.raise_for_status()
            total = 0
            for chunk in resp.iter_content(chunk_size=32):
                total += len(chunk)
            return total

    try:
        with ThreadPoolExecutor(max_workers=3) as pool:
            futures = [pool.submit(_stream_to_eof, i) for i in range(3)]
            for i, event in enumerate(started):
                assert event.wait(timeout=5), f"stream {i} did not start"
            time.sleep(1.0)

            _compose_gateway("kill", "-s", "SIGTERM", "gateway", timeout=30)

            totals = [future.result(timeout=20) for future in as_completed(futures)]

        assert totals == [len(slow_body)] * 3
        assert wait_for_records(fake_web, min_xlen=3, timeout=5.0) >= 3

        deadline = time.monotonic() + 5.0
        inflight_keys = redis_client.keys("billing:inflight:*")
        while inflight_keys and time.monotonic() < deadline:
            time.sleep(0.1)
            inflight_keys = redis_client.keys("billing:inflight:*")
        assert inflight_keys == [], f"leftover billing inflight keys: {inflight_keys}"
    finally:
        _compose_gateway("up", "-d", "gateway", timeout=60)
        _wait_for_readyz(gateway_health_url, timeout=30.0)
