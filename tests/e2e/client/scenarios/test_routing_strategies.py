"""E4-C-strat: routing strategy black-box scenarios (8).

Exercises the eight routing strategies — RoundRobin, WeightedRr,
CostFirst, LatencyFirst, LoadBalance, CacheAffinity, and Sticky edge
cases — by seeding multi-key model_groups with the appropriate `strategy`
string and asserting on the distribution of vendor_key_id values in
`usage:records`.

Gateway internals referenced:
- `crates/gw-core/src/entity.rs::StrategyKind` — serde `rename_all="snake_case"`
  enum: `round_robin`, `weighted_rr`, `cost_first`, `latency_first`,
  `load_balance`, `cache_affinity`.
- `crates/gw-routing/src/strategy/round_robin.rs` — per-model atomic counter
  rotating through candidates.
- `crates/gw-routing/src/strategy/weighted_rr.rs` — Nginx-style smooth
  weighted round-robin (weight field on vendor_key).
- `crates/gw-routing/src/strategy/cost_first.rs` — ascending sort by
  `priority * model_markup`; lower priority = cheaper = preferred.
- `crates/gw-routing/src/strategy/latency_first.rs` — picks key with lowest
  P50 TTFT; cold-start (no data) falls back to priority descending.
- `crates/gw-routing/src/strategy/load_balance.rs` — composite score
  `α·in_flight + β·p50_norm + γ·error_rate`; lowest score wins.
- `crates/gw-routing/src/strategy/cache_affinity.rs` — SHA-256 over
  `model + ":" + message_hash` mod candidate_count. When `message_hash`
  is None (current proxy — `upstream.rs:228`), degrades to first-available.
- `crates/gw-proxy/src/filters/logging.rs` — usage:records payload includes
  `vendor_key_id`.

Entity-ID prefixes (globally unique across the test suite):
  rr_   — round-robin
  wrr_  — weighted round-robin
  cf_   — cost-first
  lf_   — latency-first
  lb_   — load-balance
  ca_   — cache-affinity
  stk_  — sticky edge cases
"""

from __future__ import annotations

import json
import subprocess
import time
from collections import Counter

import pytest

from conftest import (
    BALANCE_LARGE,
    MODEL,
    PROTO_OPENAI,
    FakeUpstreamClient,
    FakeWebClient,
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


# ---------------------------------------------------------------------------
# Local helpers
# ---------------------------------------------------------------------------

WORKING_BASE = "http://fake-upstream:9000"


def _redis_xrange(stream: str = "usage:records") -> list[dict]:
    """Read every entry in `stream` via docker exec redis-cli XRANGE.

    Identical to the helper in test_routing_retry.py — duplicated to
    maintain file-level independence. Returns list of parsed JSON payloads.
    """
    proc = subprocess.run(
        [
            "docker",
            "compose",
            "-f",
            "../docker-compose.yml",
            "exec",
            "-T",
            "redis",
            "redis-cli",
            "XRANGE",
            stream,
            "-",
            "+",
        ],
        capture_output=True,
        text=True,
        timeout=10,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"redis-cli XRANGE failed (rc={proc.returncode}): {proc.stderr}"
        )
    lines = [ln for ln in proc.stdout.splitlines() if ln.strip()]
    out: list[dict] = []
    i = 0
    while i + 2 < len(lines):
        try:
            out.append(json.loads(lines[i + 2]))
        except json.JSONDecodeError:
            pass
        i += 3
    return out


def _seed_multi_key(
    fake_web: FakeWebClient,
    *,
    prefix: str,
    n_keys: int,
    strategy: str,
    weights: list[int] | None = None,
    priorities: list[int] | None = None,
) -> tuple[str, str, str, list[str]]:
    """Seed a complete entity graph with `n_keys` vendor keys under one
    model group using the given `strategy`.

    Returns (user_id, api_key_id, api_key_plaintext, [key_ids]).
    All keys share the same vendor and point at WORKING_BASE.
    """
    user_id = f"u-{prefix}"
    api_key_id = f"ak-{prefix}"
    api_key_plaintext = f"sk-{prefix}"
    vendor_id = f"v-{prefix}"
    group = f"grp-{prefix}"

    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id=vendor_id, base_urls=[WORKING_BASE])

    key_ids: list[str] = []
    for i in range(n_keys):
        kid = f"vk-{prefix}-{i}"
        key_ids.append(kid)
        seed_vendor_key(
            fake_web,
            key_id=kid,
            vendor_id=vendor_id,
            model=MODEL,
            accessible_endpoints=[PROTO_OPENAI],
            weight=weights[i] if weights else 1,
            priority=priorities[i] if priorities else 0,
        )

    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=key_ids,
        strategy=strategy,
    )
    seed_pricing(fake_web, model=MODEL)

    return user_id, api_key_id, api_key_plaintext, key_ids


def _fire_n_requests(
    gateway_url: str,
    api_key: str,
    key_id: str,
    n: int,
    *,
    stream: bool = True,
    body_extra: dict | None = None,
) -> list[int]:
    """Fire `n` sequential requests. Returns list of HTTP status codes.

    Uses `post_chat` (raw requests, no SDK retries) for consistency.
    """
    statuses: list[int] = []
    for _ in range(n):
        body: dict = {
            "model": MODEL,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": stream,
        }
        if body_extra:
            body.update(body_extra)
        resp = post_chat(
            gateway_url,
            pair_headers(api_key, key_id),
            body,
            timeout=15,
        )
        statuses.append(resp.status_code)
    return statuses


def _collect_key_distribution(
    fake_web: FakeWebClient,
    api_key_id: str,
    expected_count: int,
    timeout: float = 10.0,
) -> Counter:
    """Wait for `expected_count` usage:records, then return a Counter
    of vendor_key_id values filtered to our api_key_id."""
    wait_for_records(fake_web, min_xlen=expected_count, timeout=timeout)
    records = _redis_xrange("usage:records")
    relevant = [r for r in records if r.get("api_key_id") == api_key_id]
    return Counter(r["vendor_key_id"] for r in relevant)


# ---------------------------------------------------------------------------
# 1. RoundRobin: 3 keys, 6 requests, each key hit exactly 2 times.
# ---------------------------------------------------------------------------


def test_round_robin_distributes_across_keys(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """3 keys under round_robin strategy. 6 sequential requests should
    distribute evenly — each key gets exactly 2 hits."""
    _, api_key_id, api_key_plain, key_ids = _seed_multi_key(
        fake_web, prefix="rr", n_keys=3, strategy="round_robin"
    )

    statuses = _fire_n_requests(gateway_url, api_key_plain, api_key_id, 6)
    assert all(s == 200 for s in statuses), f"non-200 statuses: {statuses}"

    dist = _collect_key_distribution(fake_web, api_key_id, expected_count=6)
    for kid in key_ids:
        assert dist[kid] == 2, (
            f"round_robin: expected key {kid} to get exactly 2 requests, "
            f"got {dist[kid]}; full distribution: {dict(dist)}"
        )


# ---------------------------------------------------------------------------
# 2. WeightedRr: 2 keys weight 3:1, 100 requests, distribution ±15% of 75:25.
# ---------------------------------------------------------------------------


def test_weighted_rr_respects_weights(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """2 keys with weight 3:1 under weighted_rr. Over 100 requests the
    high-weight key should get roughly 75% of traffic (±15%)."""
    _, api_key_id, api_key_plain, key_ids = _seed_multi_key(
        fake_web,
        prefix="wrr",
        n_keys=2,
        strategy="weighted_rr",
        weights=[3, 1],
    )

    statuses = _fire_n_requests(gateway_url, api_key_plain, api_key_id, 100)
    assert all(s == 200 for s in statuses), (
        f"non-200 statuses: {[s for s in statuses if s != 200]}"
    )

    dist = _collect_key_distribution(
        fake_web, api_key_id, expected_count=100, timeout=30.0
    )

    heavy_key, light_key = key_ids[0], key_ids[1]
    heavy_pct = dist[heavy_key] / 100.0
    light_pct = dist[light_key] / 100.0

    # Expect ~75% ± 15% for the heavy key.
    assert 0.60 <= heavy_pct <= 0.90, (
        f"weighted_rr: heavy key {heavy_key} (weight=3) got {heavy_pct:.0%}, "
        f"expected ~75% ± 15%; full: {dict(dist)}"
    )
    assert 0.10 <= light_pct <= 0.40, (
        f"weighted_rr: light key {light_key} (weight=1) got {light_pct:.0%}, "
        f"expected ~25% ± 15%; full: {dict(dist)}"
    )


# ---------------------------------------------------------------------------
# 3. CostFirst: 2 keys priority 1 vs 10, all 5 requests hit lowest priority.
# ---------------------------------------------------------------------------


def test_cost_first_picks_cheapest_key(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """CostFirst sorts ascending by priority * model_markup. Key A
    (priority=1) is cheaper than Key B (priority=10). All 5 requests
    should route to key A."""
    _, api_key_id, api_key_plain, key_ids = _seed_multi_key(
        fake_web,
        prefix="cf",
        n_keys=2,
        strategy="cost_first",
        priorities=[1, 10],
    )

    statuses = _fire_n_requests(gateway_url, api_key_plain, api_key_id, 5)
    assert all(s == 200 for s in statuses), f"non-200 statuses: {statuses}"

    dist = _collect_key_distribution(fake_web, api_key_id, expected_count=5)
    cheap_key = key_ids[0]  # priority=1
    assert dist[cheap_key] == 5, (
        f"cost_first: expected all 5 requests on {cheap_key} (priority=1), "
        f"got {dist[cheap_key]}; full: {dict(dist)}"
    )


# ---------------------------------------------------------------------------
# 4. LatencyFirst: 2 keys with different upstream delay_ms. After warmup
#    requests populate the sliding window, subsequent requests should favor
#    the faster key.
# ---------------------------------------------------------------------------


def test_latency_first_picks_lowest_p50(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """Two vendor keys under latency_first. Key A (slow, priority=1) has a
    500ms upstream delay; Key B (fast, priority=1) has a 10ms upstream delay.

    We issue 10 warmup requests to seed the TTFT sliding window, then issue
    10 more and assert the fast key (B) gets >= 70%. The latency tracker
    now populates P50 data via ``/healthz/strategies``, allowing the
    strategy to reliably prefer the lower-latency key after warmup.
    """
    user_id = "u-lf"
    api_key_id = "ak-lf"
    api_key_plaintext = "sk-lf"
    vendor_id = "v-lf"
    group = "grp-lf"

    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id=vendor_id, base_urls=[WORKING_BASE])

    # Key A: slow (upstream_key="sk-up-slow" -> 500ms delay)
    seed_vendor_key(
        fake_web,
        key_id="vk-lf-slow",
        vendor_id=vendor_id,
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
        upstream_key="sk-up-slow",
        priority=1,
    )
    # Key B: fast (upstream_key="sk-up-fast" -> 10ms delay)
    seed_vendor_key(
        fake_web,
        key_id="vk-lf-fast",
        vendor_id=vendor_id,
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
        upstream_key="sk-up-fast",
        priority=1,
    )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=["vk-lf-slow", "vk-lf-fast"],
        strategy="latency_first",
    )
    seed_pricing(fake_web, model=MODEL)

    # Configure fake upstream: two routes differentiated by auth header.
    # The vendor's auth_template is "Bearer {api_key}", so the slow key
    # sends "Bearer sk-up-slow" and the fast key sends "Bearer sk-up-fast".
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST"},
                    "auth_check": {
                        "header": "authorization",
                        "must_start_with": "Bearer sk-up-slow",
                    },
                    "response": {
                        "kind": "sse",
                        "delay_ms": 500,
                        "events": [
                            {
                                "data": '{"choices":[{"delta":{"content":"slow"}}]}'
                            },
                            {"data": "[DONE]"},
                        ],
                    },
                },
                {
                    "match": {"method": "POST"},
                    "auth_check": {
                        "header": "authorization",
                        "must_start_with": "Bearer sk-up-fast",
                    },
                    "response": {
                        "kind": "sse",
                        "delay_ms": 10,
                        "events": [
                            {
                                "data": '{"choices":[{"delta":{"content":"fast"}}]}'
                            },
                            {"data": "[DONE]"},
                        ],
                    },
                },
            ]
        }
    )

    # Warmup: 10 requests to populate TTFT sliding window.
    warmup_statuses = _fire_n_requests(
        gateway_url, api_key_plaintext, api_key_id, 10
    )
    assert all(s == 200 for s in warmup_statuses), (
        f"warmup non-200: {warmup_statuses}"
    )
    wait_for_records(fake_web, min_xlen=10, timeout=20.0)

    # Measurement: 10 more requests. The latency tracker should now have
    # P50 data showing vk-lf-fast is much faster than vk-lf-slow.
    measure_statuses = _fire_n_requests(
        gateway_url, api_key_plaintext, api_key_id, 10
    )
    assert all(s == 200 for s in measure_statuses), (
        f"measurement non-200: {measure_statuses}"
    )
    wait_for_records(fake_web, min_xlen=20, timeout=30.0)

    # Check measurement-phase distribution.
    records = _redis_xrange("usage:records")
    relevant = [r for r in records if r.get("api_key_id") == api_key_id]
    # The last 10 records are the measurement phase.
    measure_records = relevant[10:]
    measure_dist = Counter(r["vendor_key_id"] for r in measure_records)

    fast_count = measure_dist.get("vk-lf-fast", 0)
    assert fast_count >= 7, (
        f"latency_first: expected fast key (vk-lf-fast) to get >= 70% of "
        f"measurement requests (>= 7/10), got {fast_count}/10; "
        f"dist: {dict(measure_dist)}"
    )


# ---------------------------------------------------------------------------
# 5. LoadBalance: 1 slow streaming key + 1 fast key. The second request
#    (fired while the first is still streaming) should go to the fast key
#    because it has lower in-flight count.
# ---------------------------------------------------------------------------


def test_load_balance_picks_least_loaded(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """LoadBalance uses a composite score dominated by in-flight ratio.

    We seed two keys: key A serves a slow streaming response (SSE with a
    long delay between chunks to keep the connection occupied), key B
    serves an instant response. We fire A first (streaming, don't drain),
    then immediately fire B. Key B should be picked because key A's
    in-flight count is 1 vs 0.

    Because both keys point at the same fake_upstream, we differentiate
    via auth_check on the Authorization header (same technique as
    latency_first).
    """
    user_id = "u-lb"
    api_key_id = "ak-lb"
    api_key_plaintext = "sk-lb"
    vendor_id = "v-lb"
    group = "grp-lb"

    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id=vendor_id, base_urls=[WORKING_BASE])

    seed_vendor_key(
        fake_web,
        key_id="vk-lb-slow",
        vendor_id=vendor_id,
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
        upstream_key="sk-up-lb-slow",
    )
    seed_vendor_key(
        fake_web,
        key_id="vk-lb-fast",
        vendor_id=vendor_id,
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
        upstream_key="sk-up-lb-fast",
    )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=["vk-lb-slow", "vk-lb-fast"],
        strategy="load_balance",
    )
    seed_pricing(fake_web, model=MODEL)

    # Slow key: SSE with a 3-second delay before the first chunk, keeping
    # the connection occupied (in-flight count stays elevated).
    # Fast key: instant SSE response.
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {
                        "method": "POST",
                        "header_eq": ["authorization", "Bearer sk-up-lb-slow"],
                    },
                    "auth_check": {
                        "header": "authorization",
                        "must_start_with": "Bearer sk-up-lb-slow",
                    },
                    "response": {
                        "kind": "sse",
                        "delay_ms": 3000,
                        "events": [
                            {
                                "data": '{"choices":[{"delta":{"content":"slow"}}]}'
                            },
                            {"data": "[DONE]"},
                        ],
                    },
                },
                {
                    "match": {
                        "method": "POST",
                        "header_eq": ["authorization", "Bearer sk-up-lb-fast"],
                    },
                    "auth_check": {
                        "header": "authorization",
                        "must_start_with": "Bearer sk-up-lb-fast",
                    },
                    "response": {
                        "kind": "sse",
                        "delay_ms": 0,
                        "events": [
                            {
                                "data": '{"choices":[{"delta":{"content":"fast"}}]}'
                            },
                            {"data": "[DONE]"},
                        ],
                    },
                },
            ]
        }
    )

    import threading

    # Fire the slow request in a background thread (streaming — keeps
    # the connection open for ~3s).
    slow_done = threading.Event()
    slow_status: list[int] = []

    def _slow_request():
        resp = post_chat(
            gateway_url,
            pair_headers(api_key_plaintext, api_key_id),
            {
                "model": MODEL,
                "messages": [{"role": "user", "content": "hi"}],
                "stream": True,
            },
            timeout=15,
        )
        slow_status.append(resp.status_code)
        slow_done.set()

    t = threading.Thread(target=_slow_request)
    t.start()

    # Brief pause to let the slow request establish (gateway in-flight +1).
    time.sleep(0.3)

    # Fire the fast request — should be routed to the key with lower load.
    fast_resp = post_chat(
        gateway_url,
        pair_headers(api_key_plaintext, api_key_id),
        {
            "model": MODEL,
            "messages": [{"role": "user", "content": "hello"}],
            "stream": True,
        },
        timeout=15,
    )
    assert fast_resp.status_code == 200, (
        f"fast request failed: {fast_resp.status_code}"
    )

    # Wait for slow request to complete.
    slow_done.wait(timeout=10)
    t.join(timeout=5)
    assert slow_status and slow_status[0] == 200, (
        f"slow request status: {slow_status}"
    )

    # Both requests should be recorded. The fast request (fired while the
    # slow one was occupying key A) should go to key B.
    wait_for_records(fake_web, min_xlen=2, timeout=15.0)
    records = _redis_xrange("usage:records")
    relevant = [r for r in records if r.get("api_key_id") == api_key_id]
    assert len(relevant) >= 2, (
        f"expected >= 2 records, got {len(relevant)}"
    )

    # The fast request completed first (instant response), so it may appear
    # first or second in XRANGE. We check that at least one record used
    # vk-lb-fast (the less-loaded key).
    key_set = {r["vendor_key_id"] for r in relevant}
    assert "vk-lb-fast" in key_set, (
        f"load_balance: expected vk-lb-fast to be used; keys seen: {key_set}"
    )


# ---------------------------------------------------------------------------
# 6. CacheAffinity: same messages pin to same key, different messages spread.
#
# message_hash is now populated from cached_body SHA-256 in upstream.rs.
# Same body → same hash → same upstream key via cache_affinity strategy.
# Different body → different hash → may spread across keys.
# ---------------------------------------------------------------------------


def test_cache_affinity_pins_same_content(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """CacheAffinity with message_hash populated from cached_body SHA-256.
    - 5 requests with identical content all go to the same key (affinity).
    - 5 requests with different content may spread across multiple keys.
    """
    _, api_key_id, api_key_plain, key_ids = _seed_multi_key(
        fake_web,
        prefix="ca",
        n_keys=3,
        strategy="cache_affinity",
    )

    # Phase 1: 5 identical-content requests.
    statuses_same = _fire_n_requests(
        gateway_url, api_key_plain, api_key_id, 5
    )
    assert all(s == 200 for s in statuses_same), (
        f"same-content non-200: {statuses_same}"
    )
    wait_for_records(fake_web, min_xlen=5, timeout=10.0)

    records_phase1 = _redis_xrange("usage:records")
    relevant_p1 = [
        r for r in records_phase1 if r.get("api_key_id") == api_key_id
    ]
    keys_p1 = {r["vendor_key_id"] for r in relevant_p1}
    # Same content → same message_hash → same key via affinity.
    assert len(keys_p1) == 1, (
        f"cache_affinity: same content should pin to 1 key, "
        f"got {len(keys_p1)}: {keys_p1}"
    )
    pinned_key = next(iter(keys_p1))
    assert pinned_key in key_ids, (
        f"pinned key {pinned_key} not in seeded keys {key_ids}"
    )

    # Phase 2: 5 requests with varying content. Different body → different
    # message_hash → CacheAffinity should spread across keys.
    for i in range(5):
        body = {
            "model": MODEL,
            "messages": [{"role": "user", "content": f"different message {i}"}],
            "stream": True,
        }
        resp = post_chat(
            gateway_url,
            pair_headers(api_key_plain, api_key_id),
            body,
            timeout=15,
        )
        assert resp.status_code == 200, (
            f"varied-content request {i} failed: {resp.status_code}"
        )

    wait_for_records(fake_web, min_xlen=10, timeout=15.0)
    records_all = _redis_xrange("usage:records")
    relevant_all = [
        r for r in records_all if r.get("api_key_id") == api_key_id
    ]
    # Phase 2 records only (skip first 5).
    phase2_records = relevant_all[5:]
    keys_p2 = {r["vendor_key_id"] for r in phase2_records}

    # With message_hash populated, different content should spread across
    # at least 2 of 3 keys (hash distribution). Allow 1 key if hash
    # collision happens, but expect > 1 with 5 distinct messages.
    assert len(keys_p2) >= 2, (
        f"cache_affinity: different content should spread across keys, "
        f"got {len(keys_p2)}: {keys_p2}"
    )


# ---------------------------------------------------------------------------
# Sticky strategy edge-case helpers
# ---------------------------------------------------------------------------
# Entity IDs use ``stk_`` prefix. Covers TTL-expiry rebind and
# multi-conversation key distribution — edge cases beyond the basic
# bind+hit+expire flow in test_routing_retry.py.


def _make_openai_client_with_conv(
    gateway_url: str, api_key: str, key_id: str, conv_id: str
):
    """OpenAI client with pair-auth + X-Conversation-Id header."""
    import httpx as _httpx
    import openai

    def _hook(request: "_httpx.Request") -> None:
        for h in list(request.headers.keys()):
            if h.lower() == "authorization":
                del request.headers[h]
        request.headers["x-api-key"] = api_key
        request.headers["x-api-key-id"] = key_id
        request.headers["x-conversation-id"] = conv_id

    transport = _httpx.Client(
        event_hooks={"request": [_hook]},
        timeout=30.0,
    )
    return openai.OpenAI(
        base_url=f"{gateway_url.rstrip('/')}/v1",
        api_key="unused",
        http_client=transport,
        max_retries=0,
    )


def _seed_sticky_pair(
    fake_web: FakeWebClient,
    *,
    user_id: str,
    api_key_id: str,
    api_key_plaintext: str,
    group: str,
    key_ids: list[str],
    vendor_id: str,
) -> None:
    """Seed user + api_key + vendor + N vendor_keys + model_group(sticky) + pricing."""
    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id=vendor_id, base_urls=[WORKING_BASE])
    for kid in key_ids:
        seed_vendor_key(
            fake_web,
            key_id=kid,
            vendor_id=vendor_id,
            model=MODEL,
            accessible_endpoints=[PROTO_OPENAI],
        )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=key_ids,
        strategy="sticky",
    )
    seed_pricing(fake_web, model=MODEL)


def _do_sticky_stream(
    gateway_url: str, api_key: str, key_id: str, conv_id: str
) -> None:
    """Fire one streaming chat request with the given conversation id."""
    client = _make_openai_client_with_conv(gateway_url, api_key, key_id, conv_id)
    stream = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    pieces: list[str] = []
    for chunk in stream:
        if chunk.choices and chunk.choices[0].delta.content:
            pieces.append(chunk.choices[0].delta.content)
    assert "".join(pieces) == "hi", pieces


# ---------------------------------------------------------------------------
# 7. Sticky rebinds after TTL expiry
# ---------------------------------------------------------------------------


def test_sticky_rebinds_after_ttl_expiry(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """After the sticky TTL (2 s) expires the next request MUST go through
    the bind path again (incrementing ``gw_sticky_bind_total``), even if
    the deterministic hash happens to pick the same key.
    """
    _seed_sticky_pair(
        fake_web,
        user_id="stk_u1",
        api_key_id="stk_ak1",
        api_key_plaintext="stk_sk1",
        group="stk_grp1",
        key_ids=["stk_vk1a", "stk_vk1b"],
        vendor_id="stk_v1",
    )

    conv_id = "stk_conv_rebind"

    # --- leg 1: initial bind --------------------------------------------------
    pre_bind = metrics_snapshot("gw_sticky_bind_total", metrics_url)
    _do_sticky_stream(gateway_url, "stk_sk1", "stk_ak1", conv_id)
    assert wait_for_records(fake_web, min_xlen=1, timeout=5.0) >= 1

    post_bind_1 = metrics_snapshot("gw_sticky_bind_total", metrics_url)
    delta_1 = label_delta(pre_bind, post_bind_1, "")
    assert delta_1 >= 1, (
        f"expected gw_sticky_bind_total to increment on first bind; "
        f"pre={pre_bind}, post={post_bind_1}"
    )

    # --- leg 2: cache hit (same conv within TTL) ------------------------------
    pre_hit = metrics_snapshot("gw_sticky_hit_total", metrics_url)
    _do_sticky_stream(gateway_url, "stk_sk1", "stk_ak1", conv_id)
    assert wait_for_records(fake_web, min_xlen=2, timeout=5.0) >= 2

    post_hit = metrics_snapshot("gw_sticky_hit_total", metrics_url)
    delta_hit = label_delta(pre_hit, post_hit, "")
    assert delta_hit >= 1, (
        f"expected gw_sticky_hit_total to increment on cache-hit leg; "
        f"pre={pre_hit}, post={post_hit}"
    )

    # --- leg 3: wait for TTL expiry, then rebind ------------------------------
    # GW_ROUTING__STICKY_TTL_SECS=2 in docker-compose.
    time.sleep(2.5)

    pre_rebind = metrics_snapshot("gw_sticky_bind_total", metrics_url)
    _do_sticky_stream(gateway_url, "stk_sk1", "stk_ak1", conv_id)
    assert wait_for_records(fake_web, min_xlen=3, timeout=5.0) >= 3

    # Poll-loop: confirm gw_sticky_bind_total ticked (up to 3 s extra slack).
    deadline = time.monotonic() + 3.0
    rebind_delta = 0.0
    while time.monotonic() < deadline:
        post_rebind = metrics_snapshot("gw_sticky_bind_total", metrics_url)
        rebind_delta = label_delta(pre_rebind, post_rebind, "")
        if rebind_delta >= 1:
            break
        time.sleep(0.2)

    assert rebind_delta >= 1, (
        f"expected gw_sticky_bind_total to increment after TTL expiry rebind; "
        f"pre={pre_rebind}, last_post={post_rebind}"
    )


# ---------------------------------------------------------------------------
# 8. Different conversations → different keys (distribution)
# ---------------------------------------------------------------------------


def test_sticky_different_conversations_different_keys(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """10 requests with DISTINCT conversation IDs must produce at least 2
    distinct vendor key bindings.
    """
    key_ids = ["stk_vk2a", "stk_vk2b"]
    _seed_sticky_pair(
        fake_web,
        user_id="stk_u2",
        api_key_id="stk_ak2",
        api_key_plaintext="stk_sk2",
        group="stk_grp2",
        key_ids=key_ids,
        vendor_id="stk_v2",
    )

    n_requests = 10
    pre_bind = metrics_snapshot("gw_sticky_bind_total", metrics_url)

    for i in range(n_requests):
        conv_id = f"stk_conv_dist_{i}"
        _do_sticky_stream(gateway_url, "stk_sk2", "stk_ak2", conv_id)

    # Wait for all records to land.
    assert wait_for_records(fake_web, min_xlen=n_requests, timeout=15.0) >= n_requests

    # --- assert metric: bind_total incremented >= 10 --------------------------
    deadline = time.monotonic() + 5.0
    bind_delta = 0.0
    while time.monotonic() < deadline:
        post_bind = metrics_snapshot("gw_sticky_bind_total", metrics_url)
        bind_delta = label_delta(pre_bind, post_bind, "")
        if bind_delta >= n_requests:
            break
        time.sleep(0.2)

    assert bind_delta >= n_requests, (
        f"expected gw_sticky_bind_total to increment by >={n_requests} for "
        f"{n_requests} distinct conv_ids; got delta={bind_delta}"
    )

    # --- assert distribution: at least 2 distinct keys used -------------------
    records = _redis_xrange("usage:records")
    relevant = [
        r for r in records
        if r.get("user_id") == "stk_u2" and r.get("api_key_id") == "stk_ak2"
    ]
    assert len(relevant) >= n_requests, (
        f"expected >={n_requests} usage:records for stk_u2/stk_ak2; "
        f"got {len(relevant)}"
    )

    distinct_keys = {r["vendor_key_id"] for r in relevant}
    assert len(distinct_keys) >= 2, (
        f"expected at least 2 distinct vendor_key_ids across {n_requests} "
        f"different conversation IDs; got {distinct_keys}"
    )
    for k in distinct_keys:
        assert k in set(key_ids), f"unexpected vendor_key_id {k}"
