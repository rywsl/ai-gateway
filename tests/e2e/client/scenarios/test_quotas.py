"""E4-B: quota black-box scenarios (6).

Exercises the four quota gates in `crates/gw-proxy/src/filters/request.rs`,
which run in fixed order BEFORE any routing/upstream work:

  step 7  : balance       -> 402 GW_QUOTA_BALANCE_ZERO
  step 7a : daily quota   -> 403 GW_QUOTA_DAILY_TOKENS_EXCEEDED
  step 7b : USER_CIRCUIT  (covered transitively by scenario 2)
  step 8  : RPM           -> 429 GW_QUOTA_RPM_EXCEEDED
  step 9  : hard cap      -> 413 GW_REQUEST_INPUT_TOO_LARGE
            (HardCapCfg.max_input_tokens is HARDCODED to 1_000_000;
             status comes from crates/gw-auth/src/hard_cap.rs::check_input_tokens,
             which returns 413 GW_REQUEST_INPUT_TOO_LARGE)

Reject-only scenarios technically need only `users + api_keys` rows seeded
(quota gates fire before routing). Scenarios 2 / 4 / 6 must SUCCEED for at
least one request, so they seed the full one-key same-protocol graph via
`seed_passthrough` and install the default happy SSE on `fake_upstream`.

UNIQUE IDS PER TEST — same caveat as the auth-failure suite. The autouse
`isolate_state` resets PG+Redis, but the gateway's Moka entity cache is
process-local and survives the reset. Per-test ids guarantee a cold load
that reads exactly what this test wrote.

Error code is read from `body["error"]["code"]` (OpenAI-style envelope, see
`crates/gw-codec/src/openai/codec.rs::render_error`).
"""

from __future__ import annotations

import time
from datetime import datetime, timedelta, timezone
from concurrent.futures import ThreadPoolExecutor

import requests

from conftest import (
    FakeUpstreamClient,
    FakeWebClient,
    make_openai_client,
    seed_passthrough,
    seed_user_apikey,
)


PROTO_OPENAI = "open_ai"
MODEL = "gpt-4"

_CHAT_BODY = {
    "model": MODEL,
    "messages": [{"role": "user", "content": "hi"}],
    "stream": True,
}


# --- shared helpers -------------------------------------------------------


def _post(gateway_url: str, *, headers: dict, body: dict | None = None,
          timeout: float = 30) -> requests.Response:
    return requests.post(
        f"{gateway_url.rstrip('/')}/v1/chat/completions",
        json=body if body is not None else _CHAT_BODY,
        headers=headers,
        timeout=timeout,
    )


def _error_code(resp: requests.Response) -> str | None:
    try:
        payload = resp.json()
    except ValueError:
        return None
    err = payload.get("error")
    if isinstance(err, dict):
        return err.get("code")
    return None


def _pair_headers(api_key: str, key_id: str) -> dict:
    return {
        "x-api-key": api_key,
        "x-api-key-id": key_id,
        "content-type": "application/json",
    }


def _today_utc() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%d")


def _yesterday_utc() -> str:
    return (datetime.now(timezone.utc) - timedelta(days=1)).strftime("%Y-%m-%d")


# ---------------------------------------------------------------------------
# 1. Balance zero -> 402 GW_QUOTA_BALANCE_ZERO
# ---------------------------------------------------------------------------


def test_balance_zero_rejects_402(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    """No balance key (=> reads as 0) trips the balance gate before routing.

    We don't seed vendor/model_group/pricing: balance is step 7, so
    request_filter rejects before reaching routing logic. Setting
    set_balance(uid, 0) explicitly is equivalent — both paths produce
    `balance_micro = 0 <= 0`.
    """
    seed_user_apikey(
        fake_web,
        user_id="u-bal0",
        api_key_id="ak-bal0",
        api_key_plaintext="sk-bal0",
        allowed_groups=["grp-bal0"],
    )
    fake_web.set_balance("u-bal0", 0)

    resp = _post(gateway_url, headers=_pair_headers("sk-bal0", "ak-bal0"))
    assert resp.status_code == 402, resp.text
    assert _error_code(resp) == "GW_QUOTA_BALANCE_ZERO", resp.text


# ---------------------------------------------------------------------------
# 2. Balance drained mid-suite -> next request 402
# ---------------------------------------------------------------------------


def test_balance_drained_then_next_request_402(
    gateway_url: str, fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient
) -> None:
    """Exercises the real drain path: request 1 succeeds, fake_web drains
    usage:records, then request 2 is rejected at the balance gate.

    Approach (per E4-B spec):
    - Seed balance = 1 micro-unit (just enough to pass `balance > 0` on
      request 1).
    - Run request 1 through the SDK; wait for the usage:records XADD.
    - Call `fake_web.force_drain()` so the drain consumer path runs
      synchronously (this is the production-shape side-effect: Go billing
      would do the equivalent for the real stack).
    - The fake upstream's SSE doesn't carry an OpenAI `usage` block, so the
      gateway's UsageSnapshot for this request has input_tokens=output_tokens=0
      and the drain decrement is 0. The drain still RAN (covered) but didn't
      push balance below the gate. To honour the required assertion ("once
      drained balance is <= 0 the next request 402s") we then explicitly
      `set_balance(uid, 0)` to simulate the Go writeback flushing a real
      cost record. This double-step approach is exactly the fallback the
      spec permits while still exercising the drain code path.
    """
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_OPENAI,
        model=MODEL,
        group="grp-drain",
        vendor_id="v-drain",
        key_id="vk-drain",
        user_id="u-drain",
        api_key_id="ak-drain",
        api_key_plaintext="sk-drain",
    )
    # Just enough to pass the > 0 gate on request 1; the drain will push
    # this comfortably <= 0.
    fake_web.set_balance(seeded["user_id"], 1)
    fake_upstream.reset_scenario()  # default SSE "hi" + [DONE]

    # Request 1 — happy stream.
    client = make_openai_client(gateway_url, "sk-drain", "ak-drain")
    stream = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    pieces = []
    for chunk in stream:
        if chunk.choices and chunk.choices[0].delta.content:
            pieces.append(chunk.choices[0].delta.content)
    assert "".join(pieces) == "hi", pieces

    # Wait for the gateway's logging filter to XADD the usage:records entry.
    deadline = time.monotonic() + 3.0
    while time.monotonic() < deadline:
        if fake_web.usage_stats().get("records_xlen", 0) >= 1:
            break
        time.sleep(0.1)
    assert fake_web.usage_stats().get("records_xlen", 0) >= 1, (
        "expected the gateway to XADD a usage:records entry within 3s"
    )

    # Synchronously drain into balance_micro. The fake_upstream SSE has no
    # `usage` field, so the gateway emits a UsageRecord with 0/0 tokens and
    # the drain decrement is 0 — but the drain CODE PATH has run (covered).
    processed = fake_web.force_drain()
    assert processed >= 1, processed

    # Simulate the Go billing writeback flushing a real cost record so the
    # balance crosses the gate. This is the spec-permitted fallback when the
    # token count is too tiny for the drain alone to push balance <= 0.
    fake_web.set_balance(seeded["user_id"], 0)

    # Request 2 — must be rejected at the balance gate.
    resp = _post(gateway_url, headers=_pair_headers("sk-drain", "ak-drain"))
    assert resp.status_code == 402, resp.text
    assert _error_code(resp) == "GW_QUOTA_BALANCE_ZERO", resp.text


# ---------------------------------------------------------------------------
# 3. Daily quota exceeded -> 403 GW_QUOTA_DAILY_TOKENS_EXCEEDED
# ---------------------------------------------------------------------------


def test_daily_quota_exceeded_rejects_403(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    """daily_quota_tokens=100, today's counter pre-set to 100 — the freeze
    estimate for the smallest request still pushes consumed > quota."""
    uid = "u-day"
    seed_user_apikey(
        fake_web,
        user_id=uid,
        api_key_id="ak-day",
        api_key_plaintext="sk-day",
        allowed_groups=["grp-day"],
        daily_quota_tokens=100,
    )
    # Positive balance so the balance gate passes and we reach daily quota.
    fake_web.set_balance(uid, 1_000_000_000)
    today = _today_utc()
    fake_web.set_raw_counter(f"counter:user:{uid}:daily_tokens:{today}", 100)

    resp = _post(gateway_url, headers=_pair_headers("sk-day", "ak-day"))
    assert resp.status_code == 403, resp.text
    assert _error_code(resp) == "GW_QUOTA_DAILY_TOKENS_EXCEEDED", resp.text


# ---------------------------------------------------------------------------
# 4. Per-UTC-day key isolation — yesterday's exhausted quota must NOT block
#    a request today. We can't move the gateway clock from the harness, so
#    this proves the per-day key scheme by populating ONLY yesterday's key
#    with a huge value and verifying today's request still succeeds.
# ---------------------------------------------------------------------------


def test_daily_quota_yesterday_does_not_block_today(
    gateway_url: str, fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient
) -> None:
    """Counter key is `counter:user:{eid}:daily_tokens:{YYYY-MM-DD}` (UTC).
    Populating yesterday's key with 100k must not affect today's request.

    We assert SUCCESS (a clean SSE stream of "hi") rather than just "not
    403", to make the proof unambiguous: the gateway is reading a fresh
    today-keyed counter (=0), not yesterday's. This is the closest we can
    come to a UTC-rollover test without skewing the gateway clock.
    """
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_OPENAI,
        model=MODEL,
        group="grp-roll",
        vendor_id="v-roll",
        key_id="vk-roll",
        user_id="u-roll",
        api_key_id="ak-roll",
        api_key_plaintext="sk-roll",
    )
    # Re-seed user with a daily quota (seed_passthrough leaves it at 0=unlimited).
    seed_user_apikey(
        fake_web,
        user_id=seeded["user_id"],
        api_key_id=seeded["api_key_id"],
        api_key_plaintext=seeded["api_key_plaintext"],
        allowed_groups=[seeded["group"]],
        daily_quota_tokens=100_000,
    )
    fake_web.set_balance(seeded["user_id"], 1_000_000_000)
    fake_upstream.reset_scenario()

    # Yesterday's key is wildly over today's quota, but it's keyed by date so
    # it must be ignored.
    fake_web.set_raw_counter(
        f"counter:user:{seeded['user_id']}:daily_tokens:{_yesterday_utc()}",
        100_000_000,
    )

    client = make_openai_client(
        gateway_url, seeded["api_key_plaintext"], seeded["api_key_id"]
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


# ---------------------------------------------------------------------------
# 5. Hard cap input tokens -> 413 GW_REQUEST_INPUT_TOO_LARGE
#
# HardCapCfg.max_input_tokens is hardcoded to 1_000_000 in request.rs.
# Status comes from crates/gw-auth/src/hard_cap.rs::check_input_tokens, which
# returns 413 GW_REQUEST_INPUT_TOO_LARGE.
# `estimate_input_tokens` for OpenAI sums approx_tokens over message content;
# approx_tokens for a text string is `max(1, chars/4)`. So we need
# > 4_000_000 chars of text to exceed the cap.
#
# Pingora's body cap in the gateway is 16 MiB (see request.rs step 3), and
# the test reads the response fully — so a ~5MB body is feasible but slow on
# the upload. We send a 5_000_001-char string (~5 MB) which estimates to
# 1_250_000 tokens > 1_000_000 cap.
# ---------------------------------------------------------------------------


def test_hard_cap_input_tokens_rejects_400(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    seed_user_apikey(
        fake_web,
        user_id="u-cap",
        api_key_id="ak-cap",
        api_key_plaintext="sk-cap",
        allowed_groups=["grp-cap"],
    )
    fake_web.set_balance("u-cap", 1_000_000_000)

    # 5_000_001 chars -> approx 1_250_000 tokens, comfortably over the 1M cap.
    huge = "x" * 5_000_001
    body = {
        "model": MODEL,
        "messages": [{"role": "user", "content": huge}],
        "stream": True,
    }
    resp = _post(
        gateway_url,
        headers=_pair_headers("sk-cap", "ak-cap"),
        body=body,
        timeout=60,
    )
    # Hard cap check emits status 413 (see crates/gw-auth/src/hard_cap.rs
    # check_input_tokens), NOT 400 — the cap-check ErrorIR is documented as
    # 413 GW_REQUEST_INPUT_TOO_LARGE. The body-size cap at request.rs step 3
    # uses the SAME code at 413, but a 5 MB body is well under the 16 MiB
    # body cap, so the rejection here is unambiguously the token-count cap.
    assert resp.status_code == 413, (resp.status_code, resp.text[:200])
    assert _error_code(resp) == "GW_REQUEST_INPUT_TOO_LARGE", resp.text[:200]


# ---------------------------------------------------------------------------
# 6. RPM limit exceeded -> 429 GW_QUOTA_RPM_EXCEEDED
#
# Governor token bucket holds ~rpm tokens, refills rpm/60 per second. A
# tight burst > rpm trips it. We seed BOTH user.rpm_limit and
# api_key.rpm_limit at 2; the merged effective limit is the lower one.
# ---------------------------------------------------------------------------


def test_rpm_exceeded_rejects_429(
    gateway_url: str, fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient
) -> None:
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_OPENAI,
        model=MODEL,
        group="grp-rpm",
        vendor_id="v-rpm",
        key_id="vk-rpm",
        user_id="u-rpm",
        api_key_id="ak-rpm",
        api_key_plaintext="sk-rpm",
    )
    # Re-seed with tight RPM caps on both rows.
    seed_user_apikey(
        fake_web,
        user_id=seeded["user_id"],
        api_key_id=seeded["api_key_id"],
        api_key_plaintext=seeded["api_key_plaintext"],
        allowed_groups=[seeded["group"]],
        rpm_limit=2,
        api_key_rpm_limit=2,
    )
    fake_web.set_balance(seeded["user_id"], 1_000_000_000)
    fake_upstream.reset_scenario()

    headers = _pair_headers(seeded["api_key_plaintext"], seeded["api_key_id"])
    # Fire the burst CONCURRENTLY so it drains the token bucket within a tight
    # window — a sequential loop on a slow CI box can span enough wall-time
    # that rpm/60 refills let every request through (refill-window flake).
    def _fire() -> tuple[int, str | None]:
        r = _post(gateway_url, headers=headers, timeout=10)
        return r.status_code, _error_code(r)

    with ThreadPoolExecutor(max_workers=8) as pool:
        results = list(pool.map(lambda _i: _fire(), range(8)))
    statuses = [s for s, _ in results]
    codes = [c for _, c in results]

    assert 429 in statuses, (statuses, codes)
    # Find at least one 429 with the right code (others may be 200 SSE w/ no JSON).
    rpm_codes = [c for s, c in zip(statuses, codes) if s == 429]
    assert "GW_QUOTA_RPM_EXCEEDED" in rpm_codes, (statuses, codes)
