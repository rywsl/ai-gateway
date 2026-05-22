"""E4-C: routing/retry black-box scenarios (5).

Exercises the connect-time retry hook (G10), capability mismatch (G13), and the
`Sticky` routing strategy. Each scenario seeds the entity graph and asserts on
streamed body / status / error code / `gw_retry_attempt_total` metric labels.

Gateway internals referenced (all in tree):
- `crates/gw-proxy/src/gateway.rs::fail_to_connect` — G10 retry hook. Fires
  ONLY on a Pingora-level `Err(connect)` (the connect future returned Err);
  errors that occur AFTER the TCP connect succeeds (WriteError /
  ReadError / ConnectionClosed during request-headers / response-headers
  read) go to `fail_to_proxy`, NOT `fail_to_connect`. Classification:
    Tls    → metric outcome=tls_blocked, no retry, stamp GW_UPSTREAM_TLS_ERROR
    Connect→ metric outcome=recovered while retry_count < MAX_CONNECT_RETRIES=2,
             then outcome=exhausted + stamp GW_UPSTREAM_CONNECT_FAILED.
  `ctx.upstream_error_code` is stamped but the v1 gateway has no
  `fail_to_proxy` envelope emitter — the wire response is pingora's
  default 502, NOT a JSON envelope carrying the stable code. The
  retry-bookkeeping is observable via the Prometheus metric exclusively.
- `crates/gw-proxy/src/filters/upstream.rs::pick_peer` — after fail_to_connect
  triggers `set_retry(true)`, `upstream_peer` re-runs with `retry_blacklist`
  excluding the failed `KeyId`. When the candidate set becomes empty (e.g.
  more retries than candidates) the routing path emits 503
  `GW_ROUTING_NO_AVAILABLE_CHANNEL` via `emit_envelope_and_bail` — that one
  IS rendered as a proper JSON envelope.
- `crates/gw-routing/src/strategy/sticky.rs::pick` — looks up
  `ctx.conversation_id` in a moka cache. The v1 gateway plumbs
  `conversation_id: None` (crates/gw-proxy/src/filters/upstream.rs:220),
  so `Sticky` falls back to "first available candidate" unconditionally —
  same observable as `fixed_order` for two healthy keys. Both requests
  land on the FIRST `vendor_key_ids` entry. The moka binding cache is
  inert in v1; TTL eviction is untestable.

fake_upstream's `connection_refused_first_n` semantics — IMPORTANT
---------------------------------------------------------------
This primitive does NOT model a real ECONNREFUSED at the kernel TCP level.
The fake's `serve_business` accept loop accepts the socket and then drops
it with SO_LINGER=0 to force an RST. From pingora's perspective the TCP
connect SUCCEEDED, then `write_all(request_headers)` failed with
`Connection reset by peer` (WriteError) — that surfaces through
`fail_to_proxy`, NOT `fail_to_connect`. So `gw_retry_attempt_total` is
never incremented and the gateway never tries a different key for this
particular fake's "refused" mode.

To exercise the genuine G10 retry path (the spec's intent for scenarios 1-2)
we instead point vendor_keys at an UNREACHABLE port on the fake_upstream
container (`http://fake-upstream:9999` — the container exposes 9000 and
9001 only). This produces a real ECONNREFUSED on connect, which `pingora`
surfaces as a `ConnectError` → `fail_to_connect` is invoked.
"""

from __future__ import annotations

import json
import re
import subprocess

import pytest
import requests

from conftest import (
    BALANCE_LARGE,
    MODEL,
    PROTO_OPENAI,
    FakeUpstreamClient,
    FakeWebClient,
    _error_code,
    make_openai_client,
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
# Local helpers.
# ---------------------------------------------------------------------------


# A vendor base_url that refuses TCP connect: the fake_upstream container
# only listens on 9000 (business) and 9001 (admin), so port 9999 produces
# a kernel-level ECONNREFUSED — the only failure mode that triggers
# pingora's `fail_to_connect` hook (see module docstring).
UNREACHABLE_BASE = "http://fake-upstream:9999"
WORKING_BASE = "http://fake-upstream:9000"


def _metrics_snapshot(metric: str, metrics_url: str) -> dict[str, int]:
    """Return `{label_pairs -> int_value}` for every series of `metric`.

    `label_pairs` is the raw `{a="x",b="y"}` string (or "" when unlabelled).
    Counters are floats in the Prometheus text format; we floor to int.
    """
    text = requests.get(f"{metrics_url.rstrip('/')}/metrics", timeout=5).text
    out: dict[str, int] = {}
    line_re = re.compile(
        rf"^{re.escape(metric)}(?:\{{(?P<lbl>[^}}]*)\}})?\s+(?P<v>[-+0-9eE.]+)$"
    )
    for raw in text.splitlines():
        m = line_re.match(raw)
        if m:
            out[m.group("lbl") or ""] = int(float(m.group("v")))
    return out


def _label_delta(pre: dict[str, int], post: dict[str, int], label: str) -> int:
    return post.get(label, 0) - pre.get(label, 0)


def _redis_xrange(stream: str = "usage:records") -> list[dict]:
    """Read every entry in `stream` via `docker exec redis-cli XRANGE` and
    return the decoded payloads.

    Redis is NOT exposed on the host (docker-compose only forwards 7000,
    9000/9001 fake_upstream, 8000/8080/9090 gateway, 7000 fake_web). The
    fake_web admin API exposes only XLEN counts (`/usage/stats`), not the
    entry contents. `docker exec` is the cleanest black-box read.

    Output framing: `redis-cli XRANGE` prints 3 lines per entry — id,
    field-name, field-value. The XADDs in `crates/gw-proxy/src/filters/
    logging.rs` use a single `payload` field carrying the JSON.
    """
    # Resolve redis via `docker compose ... exec redis` (service name) rather
    # than `docker exec e2e-redis-1` (container name). The container name is
    # synthesized from the compose project name, which defaults to the cwd
    # `tests/e2e` but is overridable via `-p name` or COMPOSE_PROJECT_NAME —
    # using the service name keeps this independent of project name.
    # Compose file lives one level up from `tests/e2e/client/` (the pytest
    # cwd).
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


# ---------------------------------------------------------------------------
# Seeding shortcut.
# ---------------------------------------------------------------------------


def _seed_two_vendor_one_key_each(
    fake_web: FakeWebClient,
    *,
    user_id: str,
    api_key_id: str,
    api_key_plaintext: str,
    group: str,
    keys: list[tuple[str, str, str]],
    strategy: str = "fixed_order",
) -> None:
    """Seed users/api_key + one model_group + one vendor PER (vendor_id,
    base_url) so each vendor_key has its OWN upstream URL.

    `keys` is `[(key_id, vendor_id, base_url), ...]` in priority order.
    `seed_vendor` is called for each distinct vendor_id; if the same
    vendor_id appears twice it will be overwritten by the second seed —
    callers must use distinct vendor_ids when they need distinct URLs.
    """
    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)
    seen_vendors: set[str] = set()
    for key_id, vendor_id, base_url in keys:
        if vendor_id not in seen_vendors:
            seed_vendor(fake_web, vendor_id=vendor_id, base_urls=[base_url])
            seen_vendors.add(vendor_id)
        seed_vendor_key(
            fake_web,
            key_id=key_id,
            vendor_id=vendor_id,
            model=MODEL,
            accessible_endpoints=[PROTO_OPENAI],
        )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=[k for k, _, _ in keys],
        strategy=strategy,
    )
    seed_pricing(fake_web, model=MODEL)


# ---------------------------------------------------------------------------
# 1. Two keys, key1 → unreachable vendor URL, key2 → working vendor URL with
#    happy SSE. fail_to_connect retries on key1, blacklists it, repicks key2
#    which succeeds.
# ---------------------------------------------------------------------------


def test_connect_refused_first_key_falls_through_to_second_key(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Seed TWO distinct vendors, each with one vendor_key. Vendor A's
    base_url is unreachable (port 9999 — fake_upstream listens on 9000),
    vendor B's points at the working SSE listener. With strategy=fixed_order
    and `vendor_key_ids=[k1,k2]`, the first pick is k1 (→ ECONNREFUSED),
    `fail_to_connect` blacklists k1 and bumps retry_count to 1 (still <
    MAX_CONNECT_RETRIES=2), pingora re-runs `upstream_peer`, the candidate
    filter excludes k1, k2 is picked, k2 connects, the stream returns "hi".
    """
    _seed_two_vendor_one_key_each(
        fake_web,
        user_id="u-cr1",
        api_key_id="ak-cr1",
        api_key_plaintext="sk-cr1",
        group="grp-cr1",
        keys=[
            ("vk-cr1-bad", "v-cr1-bad", UNREACHABLE_BASE),
            ("vk-cr1-good", "v-cr1-good", WORKING_BASE),
        ],
    )
    # Working vendor serves the default happy SSE (the autouse
    # isolate_state fixture already installed the baseline scenario before
    # this test ran; we don't override it).
    pre = _metrics_snapshot("gw_retry_attempt_total", metrics_url)

    client = make_openai_client(gateway_url, "sk-cr1", "ak-cr1")
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

    post = _metrics_snapshot("gw_retry_attempt_total", metrics_url)
    # `outcome="recovered"` ticks for each retry that successfully tried
    # ANOTHER key. The first attempt on `vk-cr1-bad` produces exactly one
    # `recovered` increment before the second attempt on `vk-cr1-good`
    # succeeds.
    delta_recovered = _label_delta(pre, post, 'outcome="recovered"')
    assert delta_recovered >= 1, (
        f"expected gw_retry_attempt_total{{outcome=\"recovered\"}} to "
        f"increment by >=1 for the cross-key retry; pre={pre}, post={post}"
    )


# ---------------------------------------------------------------------------
# 2. Three keys, ALL pointed at unreachable URLs. retry_count exhausts the
#    MAX_CONNECT_RETRIES=2 budget on the 3rd attempt → outcome=exhausted +
#    stamp GW_UPSTREAM_CONNECT_FAILED. We use 3 (not 2) because the
#    candidate filter empties before the exhausted branch can fire with
#    only 2 keys — see module docstring.
# ---------------------------------------------------------------------------


def test_three_keys_connect_refused_returns_5xx(
    gateway_url: str, fake_web: FakeWebClient, metrics_url: str
) -> None:
    """With 3 keys + all-failing connect, retry_count climbs 0→1→2
    (exhausted at 2) without the candidate set going empty.

    (With only 2 keys, `get_candidate_keys` returns `[]` on the 3rd
    `upstream_peer` re-run because both are blacklisted, and routing
    emits 503 GW_ROUTING_NO_AVAILABLE_CHANNEL BEFORE fail_to_connect can
    hit its `exhausted` branch — that's why this test uses three.)

    The wire status is pingora's default 5xx (502 in practice — no
    `fail_to_proxy` envelope emitter in v1) — assert on the metric and
    any 5xx instead of pinning a specific code.
    """
    _seed_two_vendor_one_key_each(
        fake_web,
        user_id="u-cr2",
        api_key_id="ak-cr2",
        api_key_plaintext="sk-cr2",
        group="grp-cr2",
        keys=[
            ("vk-cr2-a", "v-cr2-a", UNREACHABLE_BASE),
            ("vk-cr2-b", "v-cr2-b", UNREACHABLE_BASE),
            ("vk-cr2-c", "v-cr2-c", UNREACHABLE_BASE),
        ],
    )
    pre = _metrics_snapshot("gw_retry_attempt_total", metrics_url)

    resp = post_chat(
        gateway_url,
        pair_headers("sk-cr2", "ak-cr2"),
        {
            "model": MODEL,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": True,
        },
        timeout=15,
    )
    # Pingora's default for an exhausted-retry connect fallthrough is HTTP
    # 502. The stamped GW_UPSTREAM_CONNECT_FAILED code is on `ctx`, but the
    # v1 gateway has no `fail_to_proxy` body emitter — the wire body is
    # pingora's default HTML/text. We assert 5xx + the `exhausted` metric.
    assert 500 <= resp.status_code < 600, (resp.status_code, resp.text[:200])

    post = _metrics_snapshot("gw_retry_attempt_total", metrics_url)
    delta_exhausted = _label_delta(pre, post, 'outcome="exhausted"')
    assert delta_exhausted >= 1, (
        f"expected gw_retry_attempt_total{{outcome=\"exhausted\"}} to "
        f"increment when 3 keys all refuse connect; pre={pre}, post={post}"
    )


# ---------------------------------------------------------------------------
# 3. TLS handshake error → 5xx and no recovery.
#
# Pointing the vendor at https://fake-upstream:9000 makes pingora try TLS
# against a plain-HTTP listener. The TLS ClientHello fails to negotiate and
# pingora surfaces a connect-time error. Whether `classify_connect_error`
# tags it as `Tls` or `Connect` depends on the exact `ErrorType` pingora
# raises (see `gateway.rs` lines 56-65). We assert the request 5xxs AND
# the `recovered` counter does NOT increment (one key only → no retry
# could possibly succeed). The TLS-vs-Connect classification is asserted
# by checking that EITHER `tls_blocked` OR `exhausted` ticks — both are
# legitimate `fail_to_connect` outcomes for this scenario.
# ---------------------------------------------------------------------------


def test_tls_handshake_error_returns_503_no_retry(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    """SKIPPED — fake_upstream has no TLS listener and pingora's
    HTTPS-against-plain-HTTP path produces an `Upstream ConnectionClosed`
    error WHILE READING RESPONSE HEADERS (verified in
    `e2e-gateway-1` logs: `scheme: HTTPS ... while reading response headers,
    bytes already read: 0, tries: 1, retry: false`). That post-connect
    failure routes through `fail_to_proxy`, NOT `fail_to_connect`, so:
      - `classify_connect_error` never runs,
      - `gw_retry_attempt_total{outcome="tls_blocked"}` never ticks,
      - `ctx.upstream_error_code = GW_UPSTREAM_TLS_ERROR` is never stamped.

    To exercise the real TLS short-circuit we'd need a fake TLS listener
    serving a bad/self-signed cert (so pingora's TLS handshake fails
    BEFORE the TCP connect is considered "complete"). That's an
    E1-A-shaped extension to the fake_upstream — out of scope for E4-C.

    The behavior IS unit-covered in `crates/gw-proxy/src/gateway.rs`
    tests (`classify_connect_error_tls_outer` etc.) — this end-to-end
    black-box exercise must wait for the TLS-listener primitive.
    """
    pytest.skip(
        "TLS short-circuit untestable end-to-end in v1: fake_upstream has "
        "no TLS listener, and pingora's HTTPS-against-plain-HTTP error "
        "surfaces via fail_to_proxy (Upstream ConnectionClosed while "
        "reading response headers, tries=1, retry=false) — NOT via "
        "fail_to_connect, so gw_retry_attempt_total{outcome=\"tls_blocked\"} "
        "is never incremented. Re-enable once the fake_upstream gains a "
        "TLS listener that returns a bad cert; the gateway-side TLS "
        "classification is unit-covered in crates/gw-proxy/src/gateway.rs."
    )


# ---------------------------------------------------------------------------
# 4. api_key advertises required cap that no vendor_key matches → 503
#    GW_ROUTING_NO_CAPABILITY_MATCH from
#    crates/gw-proxy/src/filters/upstream.rs::pick_peer
#    (G13 — re-run the candidate filter without caps; if it would yield
#    candidates, the cap filter is what emptied the set).
#
# `candidate_matches` returns true for a candidate with EMPTY caps (non-
# restrictive). To trigger NO_CAPABILITY_MATCH we make the candidate
# advertise a DIFFERENT cap.
# ---------------------------------------------------------------------------


def test_no_key_advertises_required_capability_returns_503(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    """Required: vision (api_key.force_capabilities). Candidate advertises
    streaming_only — vision ∉ {streaming_only} → no match → 503."""
    seed_user_apikey(
        fake_web,
        user_id="u-cap-miss",
        api_key_id="ak-cap-miss",
        api_key_plaintext="sk-cap-miss",
        allowed_groups=["grp-cap-miss"],
        force_capabilities=["vision"],
        match_mode_exclusive=True,
    )
    fake_web.set_balance("u-cap-miss", BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id="v-cap-miss")
    seed_vendor_key(
        fake_web,
        key_id="vk-cap-miss",
        vendor_id="v-cap-miss",
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
        forced_capabilities=["streaming_only"],
        exclusive_capability_match=False,
    )
    seed_model_group(
        fake_web,
        group="grp-cap-miss",
        vendor_key_ids=["vk-cap-miss"],
        strategy="fixed_order",
    )
    seed_pricing(fake_web, model=MODEL)

    resp = post_chat(
        gateway_url,
        pair_headers("sk-cap-miss", "ak-cap-miss"),
        {
            "model": MODEL,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": True,
        },
        timeout=10,
    )
    assert resp.status_code == 503, (resp.status_code, resp.text[:200])
    assert _error_code(resp) == "GW_ROUTING_NO_CAPABILITY_MATCH", resp.text[:200]


# ---------------------------------------------------------------------------
# 5. Sticky strategy: two sequential streams pin to the same vendor_key.
#
# Both vendor_keys point at the working fake_upstream. The v1 gateway
# always sets `PickCtx.conversation_id = None`
# (crates/gw-proxy/src/filters/upstream.rs:220), so `Sticky::pick` takes
# the "no conversation context" branch and returns the FIRST available
# candidate — both requests land on the FIRST `vendor_key_ids` entry.
# We assert on `vendor_key_id` from `usage:records` (read via
# `docker exec redis-cli XRANGE`).
#
# TTL: DEFAULT_STICKY_TTL_SECS = 1800 (30 min) — far too long for CI.
# The moka cache binding path is also unreachable without a
# conversation_id, so the TTL eviction half is structurally untestable in
# v1; we skip it with a precise reason rather than wait 30 min.
# ---------------------------------------------------------------------------


def test_sticky_strategy_binds_user_to_same_key_then_expires(
    gateway_url: str, fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient
) -> None:
    _seed_two_vendor_one_key_each(
        fake_web,
        user_id="u-stk",
        api_key_id="ak-stk",
        api_key_plaintext="sk-stk",
        group="grp-stk",
        # Single vendor, two vendor_keys — both connect to the same
        # fake_upstream. The seed helper de-dupes vendor rows by
        # vendor_id, so passing the same `v-stk` for both rows is safe.
        keys=[
            ("vk-stk-a", "v-stk", WORKING_BASE),
            ("vk-stk-b", "v-stk", WORKING_BASE),
        ],
        strategy="sticky",
    )

    def _one_stream() -> None:
        client = make_openai_client(gateway_url, "sk-stk", "ak-stk")
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

    _one_stream()
    assert wait_for_records(fake_web, min_xlen=1, timeout=5.0) >= 1
    _one_stream()
    # Generous timeout because the logging filter's XADD is async after
    # stream completion and a previously-observed XLEN of 1 may take a
    # second or two to climb to 2.
    assert wait_for_records(fake_web, min_xlen=2, timeout=5.0) >= 2

    records = _redis_xrange("usage:records")
    relevant = [
        r
        for r in records
        if r.get("user_id") == "u-stk" and r.get("api_key_id") == "ak-stk"
    ]
    assert len(relevant) >= 2, (
        f"expected >=2 usage:records for u-stk/ak-stk; got {len(relevant)}: "
        f"{[r.get('vendor_key_id') for r in relevant]}"
    )
    vendor_keys_used = {r["vendor_key_id"] for r in relevant}
    assert len(vendor_keys_used) == 1, (
        f"sticky strategy should pin both requests to the SAME vendor_key_id; "
        f"got distinct ids {vendor_keys_used} across {len(relevant)} records"
    )

    # TTL eviction leg — skipped with precise reason.
    pytest.skip(
        "sticky TTL expiration not testable in v1: "
        "DEFAULT_STICKY_TTL_SECS=1800s is too long for CI, and the gateway "
        "plumbs conversation_id=None (crates/gw-proxy/src/filters/upstream.rs"
        ":220) so the moka bind path is structurally unreachable. Same-key "
        "binding half passed above."
    )
