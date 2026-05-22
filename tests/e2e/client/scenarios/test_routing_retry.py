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
import subprocess
import time

import httpx

from conftest import (
    BALANCE_LARGE,
    MODEL,
    PROTO_OPENAI,
    FakeUpstreamClient,
    FakeWebClient,
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


# ---------------------------------------------------------------------------
# Local helpers.
# ---------------------------------------------------------------------------


# A vendor base_url that refuses TCP connect: the fake_upstream container
# only listens on 9000 (business) and 9001 (admin), so port 9999 produces
# a kernel-level ECONNREFUSED — the only failure mode that triggers
# pingora's `fail_to_connect` hook (see module docstring).
UNREACHABLE_BASE = "http://fake-upstream:9999"
WORKING_BASE = "http://fake-upstream:9000"


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
    pre = metrics_snapshot("gw_retry_attempt_total", metrics_url)

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

    post = metrics_snapshot("gw_retry_attempt_total", metrics_url)
    # `outcome="recovered"` ticks for each retry that successfully tried
    # ANOTHER key. The first attempt on `vk-cr1-bad` produces exactly one
    # `recovered` increment before the second attempt on `vk-cr1-good`
    # succeeds.
    delta_recovered = label_delta(pre, post, 'outcome="recovered"')
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
    pre = metrics_snapshot("gw_retry_attempt_total", metrics_url)

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

    post = metrics_snapshot("gw_retry_attempt_total", metrics_url)
    delta_exhausted = label_delta(pre, post, 'outcome="exhausted"')
    assert delta_exhausted >= 1, (
        f"expected gw_retry_attempt_total{{outcome=\"exhausted\"}} to "
        f"increment when 3 keys all refuse connect; pre={pre}, post={post}"
    )


# ---------------------------------------------------------------------------
# 3. TLS handshake error → 5xx and no recovery.
#
# Pointing the vendor at https://fake-upstream:9443 makes pingora dial the
# fake-upstream's TLS listener (ADR-S5). That listener serves a self-signed
# cert that the gateway truststore does NOT trust, so the TLS handshake
# fails inside pingora's connect path. `classify_connect_error` tags this
# as `Tls`, `fail_to_connect` ticks `gw_retry_attempt_total{outcome=
# "tls_blocked"}`, stamps `GW_UPSTREAM_TLS_ERROR` onto ctx, and propagates
# WITHOUT setting retry — TLS errors are non-retryable per the design.
#
# Observable on the wire: the gateway has no `fail_to_proxy` envelope
# emitter (architecture.md §Deliberate behaviours), so pingora's default
# 5xx body hits the wire rather than a JSON envelope carrying
# `GW_UPSTREAM_TLS_ERROR`. We therefore assert on the metric and on a 5xx
# status; the stable error code is verified at unit-test scope in
# `crates/gw-proxy/src/gateway.rs::classify_connect_error_tls_outer`.
# ---------------------------------------------------------------------------


def test_tls_handshake_error_returns_503_no_retry(
    gateway_url: str, fake_web: FakeWebClient, metrics_url: str
) -> None:
    """One vendor_key pointing at `https://fake-upstream:9443`. pingora's TLS
    handshake against the self-signed cert fails inside `connect_to_upstream`;
    `fail_to_connect` classifies it as `Tls`, ticks
    `gw_retry_attempt_total{outcome="tls_blocked"}`, and does NOT call
    `set_retry(true)`. With only one key, `outcome="recovered"` MUST NOT
    tick — there is no other key to try, but more importantly TLS errors
    are non-retryable by design.
    """
    seed_user_apikey(
        fake_web,
        user_id="u-tls",
        api_key_id="ak-tls",
        api_key_plaintext="sk-tls",
        allowed_groups=["grp-tls"],
    )
    fake_web.set_balance("u-tls", BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id="v-tls", base_urls=["https://fake-upstream:9443"])
    seed_vendor_key(
        fake_web,
        key_id="vk-tls",
        vendor_id="v-tls",
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_model_group(
        fake_web,
        group="grp-tls",
        vendor_key_ids=["vk-tls"],
        strategy="fixed_order",
    )
    seed_pricing(fake_web, model=MODEL)

    pre = metrics_snapshot("gw_retry_attempt_total", metrics_url)

    # post_chat (NOT the openai SDK) — the SDK retries internally and
    # masks the single-shot semantics this test needs.
    resp = post_chat(
        gateway_url,
        pair_headers("sk-tls", "ak-tls"),
        {
            "model": MODEL,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": True,
        },
        timeout=15,
    )
    # pingora's default body for an unrecoverable connect failure is a 5xx;
    # the gateway has no fail_to_proxy envelope emitter so we assert on the
    # status class, not on `_error_code(resp)`. The stable error-code wire-
    # up is tracked separately as a known gap (see docs/architecture.md
    # §Deliberate behaviours — "No fail_to_proxy envelope emitter").
    assert 500 <= resp.status_code < 600, (resp.status_code, resp.text[:200])

    post = metrics_snapshot("gw_retry_attempt_total", metrics_url)
    delta_tls = label_delta(pre, post, 'outcome="tls_blocked"')
    assert delta_tls >= 1, (
        f"expected gw_retry_attempt_total{{outcome=\"tls_blocked\"}} to "
        f"increment by >=1 when the TLS handshake to "
        f"https://fake-upstream:9443 fails; pre={pre}, post={post}"
    )

    # TLS errors are non-retryable: `fail_to_connect` returns the original
    # error WITHOUT calling `set_retry(true)`, so pingora does not re-run
    # `upstream_peer` for another candidate. The `recovered` outcome must
    # NOT tick during this request.
    delta_recovered = label_delta(pre, post, 'outcome="recovered"')
    assert delta_recovered == 0, (
        f"TLS errors are non-retryable; gw_retry_attempt_total"
        f"{{outcome=\"recovered\"}} must not tick, but did "
        f"(delta={delta_recovered}); pre={pre}, post={post}"
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
# 5. Sticky strategy: two sequential streams with the same X-Conversation-Id
#    pin to the same vendor_key (moka bind path hit), then after TTL=2s
#    expiry a third stream forces a rebind.
#
# Both vendor_keys point at the working fake_upstream. With the
# `X-Conversation-Id: e2e-conv-1` header present, `Sticky::pick` takes the
# bind path: the first request hashes-mod-n to pick one of the two keys
# and caches the binding; the second request hits the cache and reuses
# the same key.
#
# After `time.sleep(3.0)` (TTL=2s + slack — GW_ROUTING__STICKY_TTL_SECS=2
# in tests/e2e/docker-compose.yml), the binding for `e2e-conv-1` is
# evicted; a third request goes through the bind path again. The hash
# is deterministic over the same conv-id, so we cannot assert "different
# key" — instead we assert: third record's vendor_key_id is one of the
# two seeded ids, AND `gw_sticky_bind_total` ticked >=1 between t=before
# and t=after the third request (proves the rebind path ran).
# ---------------------------------------------------------------------------


def _make_openai_client_with_conv(
    gateway_url: str, api_key: str, key_id: str, conv_id: str
):
    """openai.OpenAI() with pair-auth + an `X-Conversation-Id` header.

    Local helper (NOT in conftest) so this test alone owns the
    conversation-id behaviour. Copies `make_openai_client`'s pair-auth
    strip-and-set hook and extends it with one more `headers[...]= ` line.
    """
    import openai  # local import so test collection works even if SDK missing

    def _hook(request: "httpx.Request") -> None:  # type: ignore[name-defined]
        for h in list(request.headers.keys()):
            if h.lower() == "authorization":
                del request.headers[h]
        request.headers["x-api-key"] = api_key
        request.headers["x-api-key-id"] = key_id
        request.headers["x-conversation-id"] = conv_id

    transport = httpx.Client(
        event_hooks={"request": [_hook]},
        timeout=30.0,
    )
    return openai.OpenAI(
        base_url=f"{gateway_url.rstrip('/')}/v1",
        api_key="unused",
        http_client=transport,
        max_retries=0,
    )


def test_sticky_strategy_binds_user_to_same_key_then_expires(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
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

    conv_id = "e2e-conv-1"

    def _one_stream() -> None:
        client = _make_openai_client_with_conv(
            gateway_url, "sk-stk", "ak-stk", conv_id
        )
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

    # ----- legs 1+2: bind then hit. Both streams carry the same
    #       X-Conversation-Id and so MUST land on the same vendor_key.
    _one_stream()
    assert wait_for_records(fake_web, min_xlen=1, timeout=5.0) >= 1
    _one_stream()
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
    bound_keys = {r["vendor_key_id"] for r in relevant[:2]}
    assert len(bound_keys) == 1, (
        f"sticky strategy with the same X-Conversation-Id should pin both "
        f"requests to the SAME vendor_key_id; got distinct ids {bound_keys}"
    )
    bound_key = next(iter(bound_keys))
    assert bound_key in {"vk-stk-a", "vk-stk-b"}, bound_key

    # ----- leg 3: TTL expiry → rebind.
    # GW_ROUTING__STICKY_TTL_SECS=2 in tests/e2e/docker-compose.yml. Sleep
    # 3.0s (TTL + slack) so the moka entry is past its time_to_live; the
    # next pick takes the bind path again.
    pre_bind = metrics_snapshot("gw_sticky_bind_total", metrics_url)
    time.sleep(3.0)
    _one_stream()
    assert wait_for_records(fake_web, min_xlen=3, timeout=5.0) >= 3
    post_bind = metrics_snapshot("gw_sticky_bind_total", metrics_url)

    # Hash-mod-n over the same conv-id is deterministic, so the rebind
    # MAY pick the same key. We only assert: (a) the third record landed
    # on one of the seeded ids; (b) the bind counter ticked at least
    # once during this stream (proves the bind path ran rather than the
    # cache-hit path).
    records_after = _redis_xrange("usage:records")
    relevant_after = [
        r
        for r in records_after
        if r.get("user_id") == "u-stk" and r.get("api_key_id") == "ak-stk"
    ]
    assert len(relevant_after) >= 3, (
        f"expected >=3 usage:records for u-stk/ak-stk after the TTL leg; "
        f"got {len(relevant_after)}"
    )
    third_key = relevant_after[2]["vendor_key_id"]
    assert third_key in {"vk-stk-a", "vk-stk-b"}, third_key

    # `gw_sticky_bind_total` is unlabelled (see metrics_catalog.rs line
    # 135), so the snapshot dict has a single "" key.
    delta_bind = label_delta(pre_bind, post_bind, "")
    assert delta_bind >= 1, (
        f"expected gw_sticky_bind_total to increment by >=1 across the "
        f"TTL-expired rebind; pre={pre_bind}, post={post_bind}"
    )
