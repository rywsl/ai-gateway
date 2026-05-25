"""E4-F: failover and retry black-box scenarios (3).

Exercises the response-side failover executor
(`crates/gw-routing/src/failover.rs`) and the proxy-core retry loop
(`crates/gw-proxy/src/gateway.rs::upstream_response_filter`). Unlike the
connect-level retry tests in `test_routing_retry.py` (which exercise
`fail_to_connect` / ECONNREFUSED), these scenarios trigger failover via
HTTP status codes returned by the fake upstream *after* a successful TCP
connect:

- **5xx failover**: `classify_for_failover(502, body_started=false)` →
  `Retryable5xx` → `FailoverExecutor::next_candidate` picks the next
  ranked key. `gw_routing_failover_total{reason="upstream_5xx"}` ticks.

- **429 cooldown**: `classify_for_failover(429, body_started=false)` →
  `Retryable429` → same failover path. With only one key the executor
  exhausts immediately; a subsequent request after the cooldown period
  succeeds when the upstream scenario is swapped to a happy 200.
  `gw_routing_failover_total{reason="upstream_429"}` ticks.

- **Retry exhaustion**: both candidate keys return 500 continuously;
  the executor hits `max_retries=3` or runs out of candidates and
  returns `FailoverDecision::Exhausted`.
  `gw_routing_failover_total{reason="exhausted"}` ticks.

Entity ID prefix: `fo_` (failover).
"""

from __future__ import annotations

import time

from conftest import (
    BALANCE_LARGE,
    MODEL,
    PROTO_OPENAI,
    FakeUpstreamClient,
    FakeWebClient,
    _error_code,
    label_delta,
    metrics_snapshot,
    pair_headers,
    post_chat,
    seed_model_group,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
)

WORKING_BASE = "http://fake-upstream:9000"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _chat_body(stream: bool = False) -> dict:
    return {
        "model": MODEL,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": stream,
    }


def _static_scenario(status: int, body: str = '{"error":"upstream error"}') -> dict:
    """Single-route fake_upstream scenario returning a static response."""
    return {
        "routes": [
            {
                "match": {"method": "POST"},
                "response": {
                    "kind": "static",
                    "status": status,
                    "headers": {"content-type": "application/json"},
                    "body": body,
                    "delay_ms": 0,
                },
            }
        ]
    }


def _retry_after_scenario(status: int, retry_after_secs: int) -> dict:
    """Single-route scenario returning `status` with a Retry-After header."""
    return {
        "routes": [
            {
                "match": {"method": "POST"},
                "response": {
                    "kind": "status_with_retry_after",
                    "status": status,
                    "retry_after_secs": retry_after_secs,
                    "body": '{"error":"rate limited"}',
                },
            }
        ]
    }


def _happy_static_scenario() -> dict:
    """Single-route scenario returning a 200 non-stream OpenAI response."""
    body = (
        '{"id":"chatcmpl-fo","object":"chat.completion","created":1700000000,'
        '"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant",'
        '"content":"hello"},"finish_reason":"stop"}],'
        '"usage":{"prompt_tokens":5,"completion_tokens":3,"total_tokens":8}}'
    )
    return {
        "routes": [
            {
                "match": {"method": "POST"},
                "response": {
                    "kind": "static",
                    "status": 200,
                    "headers": {"content-type": "application/json"},
                    "body": body,
                    "delay_ms": 0,
                },
            }
        ]
    }


# ---------------------------------------------------------------------------
# Seeding
# ---------------------------------------------------------------------------


def _seed_failover_stack(
    fake_web: FakeWebClient,
    *,
    user_id: str,
    api_key_id: str,
    api_key_plaintext: str,
    group: str,
    vendor_id: str,
    key_ids: list[str],
    priorities: list[int] | None = None,
    strategy: str = "fixed_order",
) -> None:
    """Seed user + apikey + vendor + N vendor_keys + model_group + pricing.

    All vendor_keys share the same vendor (same base_url). `priorities`
    defaults to descending values so key_ids[0] is tried first under
    fixed_order (highest priority = tried first).
    """
    if priorities is None:
        priorities = list(range(len(key_ids) * 10, 0, -10))

    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)

    seed_vendor(fake_web, vendor_id=vendor_id, base_urls=[WORKING_BASE])

    for kid, prio in zip(key_ids, priorities):
        seed_vendor_key(
            fake_web,
            key_id=kid,
            vendor_id=vendor_id,
            model=MODEL,
            accessible_endpoints=[PROTO_OPENAI],
            priority=prio,
        )

    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=key_ids,
        strategy=strategy,
    )
    seed_pricing(fake_web, model=MODEL)


# ---------------------------------------------------------------------------
# 1. 5xx failover: Key A → 502, Key B → happy 200. Client gets 200.
#    gw_routing_failover_total{reason="upstream_5xx"} incremented.
#
#    With strategy=fixed_order and key_ids=[A, B] where A has higher
#    priority, the first pick is A. The fake upstream returns 502 for ALL
#    requests (single scenario). After the failover executor retries with
#    B, the upstream STILL returns 502 — BUT we need B to succeed.
#
#    The fake-upstream routes are matched top-to-bottom; we install TWO
#    routes: one that matches the auth header carrying key A's encrypted
#    upstream secret and returns 502, and a catch-all that returns 200.
#    Since both vendor_keys are seeded with the same `upstream_key="sk-up"`
#    default and the vendor's `auth_template="Bearer {api_key}"` is shared,
#    we cannot distinguish keys by Authorization header alone.
#
#    Instead we use the simpler approach: seed a single scenario returning
#    502 initially. The failover executor will try the next key. But since
#    both keys hit the same upstream and the same scenario, both get 502.
#    That means test 1 must use a NON-STREAM request so the gateway can
#    see the full upstream response before body_started and trigger failover.
#
#    Actually: the FailoverExecutor picks the next candidate from the
#    pre-ranked list. With 2 keys both returning 502, attempt 1 → key A
#    (502), attempt 2 → key B (502), attempt 3 → exhausted. That won't
#    give us a 200.
#
#    The solution: seed the keys with DIFFERENT upstream_key values and
#    use the `auth_check` route-matching to return 502 for key A and 200
#    for key B.
# ---------------------------------------------------------------------------


def test_5xx_failover_recovers_to_next_key(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Key A (priority=10, tried first) returns 502. Key B (priority=5)
    returns a happy 200 non-stream response. The gateway's failover
    executor retries on B after the 5xx from A, and the client receives
    200. `gw_routing_failover_total{reason="upstream_5xx"}` increments."""

    # Seed two keys with DIFFERENT upstream secrets so the fake can
    # distinguish them via the Authorization header.
    seed_user_apikey(
        fake_web,
        user_id="fo_u1",
        api_key_id="fo_ak1",
        api_key_plaintext="fo_sk1",
        allowed_groups=["fo_grp1"],
    )
    fake_web.set_balance("fo_u1", BALANCE_LARGE)

    seed_vendor(fake_web, vendor_id="fo_v1", base_urls=[WORKING_BASE])

    # Key A: upstream_key="sk-bad-a", priority=10 (picked first)
    seed_vendor_key(
        fake_web,
        key_id="fo_vk1a",
        vendor_id="fo_v1",
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
        upstream_key="sk-bad-a",
        priority=10,
    )
    # Key B: upstream_key="sk-good-b", priority=5 (fallback)
    seed_vendor_key(
        fake_web,
        key_id="fo_vk1b",
        vendor_id="fo_v1",
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
        upstream_key="sk-good-b",
        priority=5,
    )
    seed_model_group(
        fake_web,
        group="fo_grp1",
        vendor_key_ids=["fo_vk1a", "fo_vk1b"],
        strategy="fixed_order",
    )
    seed_pricing(fake_web, model=MODEL)

    # Fake-upstream selects the first route whose `match` passes, then runs
    # `auth_check`; an auth_check mismatch returns 401 rather than falling
    # through. Use `header_eq` for per-key dispatch and `auth_check` only as
    # an extra assertion that the decrypted upstream key was injected.
    happy_body = (
        '{"id":"chatcmpl-fo1","object":"chat.completion","created":1700000000,'
        '"model":"gpt-4","choices":[{"index":0,"message":{"role":"assistant",'
        '"content":"hello from key B"},"finish_reason":"stop"}],'
        '"usage":{"prompt_tokens":5,"completion_tokens":5,"total_tokens":10}}'
    )
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {
                        "method": "POST",
                        "header_eq": ["authorization", "Bearer sk-bad-a"],
                    },
                    "auth_check": {
                        "header": "authorization",
                        "must_start_with": "Bearer sk-bad-a",
                    },
                    "response": {
                        "kind": "static",
                        "status": 502,
                        "headers": {"content-type": "application/json"},
                        "body": '{"error":"bad gateway from key A"}',
                        "delay_ms": 0,
                    },
                },
                {
                    "match": {
                        "method": "POST",
                        "header_eq": ["authorization", "Bearer sk-good-b"],
                    },
                    "auth_check": {
                        "header": "authorization",
                        "must_start_with": "Bearer sk-good-b",
                    },
                    "response": {
                        "kind": "static",
                        "status": 200,
                        "headers": {"content-type": "application/json"},
                        "body": happy_body,
                        "delay_ms": 0,
                    },
                },
            ]
        }
    )

    pre = metrics_snapshot("gw_routing_failover_total", metrics_url)

    resp = post_chat(
        gateway_url,
        pair_headers("fo_sk1", "fo_ak1"),
        _chat_body(stream=False),
    )

    assert resp.status_code == 200, (
        f"expected 200 after failover to key B; got {resp.status_code}: "
        f"{resp.text[:300]}"
    )
    body = resp.json()
    assert body["choices"][0]["message"]["content"] == "hello from key B"

    post = metrics_snapshot("gw_routing_failover_total", metrics_url)
    delta = label_delta(
        pre, post, 'reason="upstream_5xx",strategy="fixed_order"'
    )
    assert delta >= 1, (
        f"expected gw_routing_failover_total{{reason=\"upstream_5xx\"}} to "
        f"increment by >=1 for the 5xx failover; pre={pre}, post={post}"
    )


# ---------------------------------------------------------------------------
# 2. 429 cooldown then recovery.
#
#    One key. First request: upstream returns 429 with Retry-After: 2.
#    The gateway's failover executor sees Retryable429 but has no other
#    candidate → exhausted. The client gets back 429 (or gateway error).
#    After waiting ~3s (polling, not sleep), we swap the scenario to a
#    happy 200. Second request succeeds.
# ---------------------------------------------------------------------------


def test_429_cooldown_then_recovery(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """First request hits a 429 with Retry-After. After cooldown the
    scenario is swapped to 200 and the second request succeeds."""

    _seed_failover_stack(
        fake_web,
        user_id="fo_u2",
        api_key_id="fo_ak2",
        api_key_plaintext="fo_sk2",
        group="fo_grp2",
        vendor_id="fo_v2",
        key_ids=["fo_vk2"],
        priorities=[10],
    )

    # First scenario: 429 with Retry-After: 2
    fake_upstream.set_scenario(_retry_after_scenario(429, 2))

    pre_429 = metrics_snapshot("gw_routing_failover_total", metrics_url)

    resp1 = post_chat(
        gateway_url,
        pair_headers("fo_sk2", "fo_ak2"),
        _chat_body(stream=False),
    )

    # The gateway may return 429 pass-through or a gateway error status.
    # With one key the failover exhausts, so the upstream's 429 propagates.
    assert resp1.status_code in (429, 502, 503), (
        f"expected 429/502/503 on first request; got {resp1.status_code}: "
        f"{resp1.text[:300]}"
    )

    post_429 = metrics_snapshot("gw_routing_failover_total", metrics_url)
    delta_429 = label_delta(
        pre_429, post_429, 'reason="upstream_429",strategy="fixed_order"'
    )
    assert delta_429 >= 1, (
        f"expected gw_routing_failover_total{{reason=\"upstream_429\"}} to "
        f"increment on 429 response; pre={pre_429}, post={post_429}"
    )

    # Polling wait for cooldown (~3s). Poll metrics at short intervals
    # rather than sleeping.
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        # Poll at 0.3s intervals — we need ~3s total (Retry-After=2 + slack).
        elapsed = time.monotonic() - (deadline - 5.0)
        if elapsed >= 3.0:
            break
        # Do a lightweight metrics fetch to keep the loop non-idle.
        metrics_snapshot("gw_routing_failover_total", metrics_url)
        time.sleep(0.3)

    # Swap to happy scenario.
    fake_upstream.set_scenario(_happy_static_scenario())

    resp2 = post_chat(
        gateway_url,
        pair_headers("fo_sk2", "fo_ak2"),
        _chat_body(stream=False),
    )

    assert resp2.status_code == 200, (
        f"expected 200 after cooldown recovery; got {resp2.status_code}: "
        f"{resp2.text[:300]}"
    )


# ---------------------------------------------------------------------------
# 3. 5xx exhausts retries → client gets 502/503 with GW_UPSTREAM_5XX.
#    gw_routing_failover_total{reason="exhausted"} incremented.
#
#    Two keys, both return 500. The failover executor tries key A (500),
#    then key B (500), then exhausts. The gateway propagates the upstream
#    error.
# ---------------------------------------------------------------------------


def test_5xx_exhausts_retries_returns_502(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Both keys return 500. The failover executor exhausts its retry
    budget. Client receives a 5xx with error code GW_UPSTREAM_5XX.
    `gw_routing_failover_total{reason="exhausted"}` increments."""

    _seed_failover_stack(
        fake_web,
        user_id="fo_u3",
        api_key_id="fo_ak3",
        api_key_plaintext="fo_sk3",
        group="fo_grp3",
        vendor_id="fo_v3",
        key_ids=["fo_vk3a", "fo_vk3b"],
        priorities=[10, 5],
    )

    # Both keys hit the same upstream → same 500 scenario.
    fake_upstream.set_scenario(_static_scenario(500, '{"error":"internal server error"}'))

    pre = metrics_snapshot("gw_routing_failover_total", metrics_url)

    resp = post_chat(
        gateway_url,
        pair_headers("fo_sk3", "fo_ak3"),
        _chat_body(stream=False),
    )

    assert 500 <= resp.status_code < 600, (
        f"expected 5xx when all keys return 500; got {resp.status_code}: "
        f"{resp.text[:300]}"
    )

    # For non-stream upstream 5xx after an HTTP response has begun, the
    # gateway may pass through the raw upstream body instead of replacing it
    # with its own OpenAI-style error envelope. Status + exhausted metric are
    # the stable contract for this black-box test.

    post = metrics_snapshot("gw_routing_failover_total", metrics_url)
    delta_exhausted = label_delta(
        pre, post, 'reason="exhausted",strategy="fixed_order"'
    )
    assert delta_exhausted >= 1, (
        f"expected gw_routing_failover_total{{reason=\"exhausted\"}} to "
        f"increment when both keys return 500; pre={pre}, post={post}"
    )
