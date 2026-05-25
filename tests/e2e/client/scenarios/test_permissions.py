"""E4-F: permission merge and ReDoS budget black-box scenarios (5).

Exercises two capability surfaces:

1. **Five-tier permission merge** (`crates/gw-auth/src/merge.rs`):
   - `allowed_models`: INTERSECTION across tiers (empty = unrestricted, skip tier).
   - `rpm_limit`: min-positive (smallest non-zero value across tiers wins).
   - `markup`: PRODUCT (multiply all tier markups together).

2. **ReDoS budget guard** (`crates/gw-auth/src/model_permissions.rs`):
   - `BUDGET_MS = 100` — per-call regex evaluation budget.
   - `TIMEOUT_THRESHOLD = 10` — after 10 timeouts per ApiKey, auto-disable.
   - Metric: `gw_redos_timeout_total{api_key_id="..."}`.
   - When auto-disabled, model match returns `None` → permission denied.

   NOTE: The Rust `regex` crate uses RE2-style linear-time matching, so
   true catastrophic backtracking (`(a+)+$`) cannot occur. The budget guard
   is observational (post-check timestamp), not preemptive. These tests
   validate the auto-disable pathway IF it were triggered by an
   implementation change (e.g. switching to a PCRE engine) or by a
   sufficiently expensive DFA construction. In practice the pathological
   pattern compiles fine under RE2 and matches in microseconds, so tests 4
   and 5 are marked `xfail` until the `model_whitelist_regex` entity field
   lands on `ApiKeyEntry` and a preemptive budget mechanism is wired.

Error codes under test:
  - 403 `GW_REQUEST_MODEL_NOT_FOUND` — model not in merged `allowed_models`.
  - 429 `GW_QUOTA_RPM_EXCEEDED` — merged RPM exceeded.

Entity ID prefixes: `pm_` (permission merge), `rd_` (ReDoS).
"""

from __future__ import annotations

import json
import time
from concurrent.futures import ThreadPoolExecutor

import pytest
import requests

from conftest import (
    BALANCE_LARGE,
    MODEL,
    PROTO_OPENAI,
    FakeUpstreamClient,
    FakeWebClient,
    UsageRecordsConsumer,
    _error_code,
    label_delta,
    make_openai_client,
    metrics_snapshot,
    pair_headers,
    post_chat,
    seed_model_group,
    seed_passthrough,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
    wait_for_records,
)


_CHAT_BODY = {
    "model": MODEL,
    "messages": [{"role": "user", "content": "hi"}],
    "stream": True,
}


def _post(gateway_url: str, *, headers: dict, body: dict | None = None,
          timeout: float = 10) -> requests.Response:
    return post_chat(
        gateway_url,
        headers,
        body if body is not None else _CHAT_BODY,
        timeout=timeout,
    )


def _pair(api_key: str, key_id: str) -> dict:
    return pair_headers(api_key, key_id)


# ---------------------------------------------------------------------------
# Happy-path seed helper — minimal one-key OpenAI passthrough graph.
# ---------------------------------------------------------------------------

def _seed_happy_path(
    fake_web: FakeWebClient,
    *,
    prefix: str,
    model: str = MODEL,
    user_markup: float = 1.0,
    api_key_markup: float = 1.0,
    rpm_limit: int = 1000,
    api_key_rpm_limit: int = 0,
    allowed_models: list[str] | None = None,
    api_key_allowed_models: list[str] | None = None,
    user_group_ids: list[str] | None = None,
    model_markup: float = 1.0,
) -> dict:
    """Seed user + api_key + vendor + vendor_key + model_group + pricing.

    Returns a dict with the entity ids for caller convenience.
    """
    uid = f"u-{prefix}"
    akid = f"ak-{prefix}"
    ak_plain = f"sk-{prefix}"
    vid = f"v-{prefix}"
    vkid = f"vk-{prefix}"
    grp = f"grp-{prefix}"

    seed_user_apikey(
        fake_web,
        user_id=uid,
        api_key_id=akid,
        api_key_plaintext=ak_plain,
        allowed_groups=[grp],
        allowed_models=allowed_models,
        api_key_allowed_models=api_key_allowed_models,
        rpm_limit=rpm_limit,
        api_key_rpm_limit=api_key_rpm_limit,
        user_markup=user_markup,
        api_key_markup=api_key_markup,
        user_group_ids=user_group_ids,
    )
    fake_web.set_balance(uid, BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id=vid)
    seed_vendor_key(
        fake_web,
        key_id=vkid,
        vendor_id=vid,
        model=model,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_model_group(fake_web, group=grp, vendor_key_ids=[vkid])
    seed_pricing(fake_web, model=model, model_markup=model_markup)

    return {
        "user_id": uid,
        "api_key_id": akid,
        "api_key_plaintext": ak_plain,
        "vendor_id": vid,
        "key_id": vkid,
        "group": grp,
        "model": model,
    }


# =========================================================================
# 1. Model intersection: user allowed_models ∩ api_key allowed_models
# =========================================================================


def test_permission_merge_model_intersection(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """User allows [gpt-4, gpt-3.5-turbo], api-key allows [gpt-4, claude-3].

    Intersection = {gpt-4}. Only gpt-4 must succeed; gpt-3.5-turbo and
    claude-3 must be rejected with 403 GW_REQUEST_MODEL_NOT_FOUND.

    We seed vendor_key + model_group + pricing only for gpt-4 (the happy
    model). The rejected models never reach routing, so they don't need
    upstream infrastructure.
    """
    prefix = "pm_model"

    # Seed with model intersection: user has [gpt-4, gpt-3.5-turbo],
    # api-key has [gpt-4, claude-3]. Intersection = {gpt-4}.
    _seed_happy_path(
        fake_web,
        prefix=prefix,
        model="gpt-4",
        allowed_models=["gpt-4", "gpt-3.5-turbo"],
        api_key_allowed_models=["gpt-4", "claude-3"],
    )
    fake_upstream.reset_scenario()

    # gpt-4 — must succeed (it's in the intersection AND has upstream infra).
    client = make_openai_client(gateway_url, f"sk-{prefix}", f"ak-{prefix}")
    stream = client.chat.completions.create(
        model="gpt-4",
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    pieces = [c.choices[0].delta.content for c in stream
              if c.choices and c.choices[0].delta.content]
    assert "".join(pieces) == "hi", pieces

    # gpt-3.5-turbo — in user but NOT in api-key → rejected.
    body_35 = {"model": "gpt-3.5-turbo",
               "messages": [{"role": "user", "content": "hi"}], "stream": True}
    resp = _post(gateway_url, headers=_pair(f"sk-{prefix}", f"ak-{prefix}"),
                 body=body_35)
    assert resp.status_code == 403, resp.text
    assert _error_code(resp) == "GW_REQUEST_MODEL_NOT_FOUND", resp.text

    # claude-3 — in api-key but NOT in user → rejected.
    body_c3 = {"model": "claude-3",
               "messages": [{"role": "user", "content": "hi"}], "stream": True}
    resp = _post(gateway_url, headers=_pair(f"sk-{prefix}", f"ak-{prefix}"),
                 body=body_c3)
    assert resp.status_code == 403, resp.text
    assert _error_code(resp) == "GW_REQUEST_MODEL_NOT_FOUND", resp.text


# =========================================================================
# 2. RPM min-positive: user rpm=100, api-key rpm=50 → effective = 50
# =========================================================================


def test_permission_merge_rpm_min_positive(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """User rpm_limit=100, api_key rpm_limit=50. Merged effective = 50.

    Fire 51 requests concurrently; at least one must get 429
    GW_QUOTA_RPM_EXCEEDED. The token-bucket holds ~rpm tokens and refills
    at rpm/60 per second, so a tight concurrent burst exceeding 50 must
    trip the gate.
    """
    prefix = "pm_rpm"
    _seed_happy_path(
        fake_web,
        prefix=prefix,
        rpm_limit=100,
        api_key_rpm_limit=50,
    )
    fake_upstream.reset_scenario()

    headers = _pair(f"sk-{prefix}", f"ak-{prefix}")

    def _fire() -> tuple[int, str | None]:
        r = _post(gateway_url, headers=headers, timeout=10)
        return r.status_code, _error_code(r)

    # Fire 51 concurrent requests — the 50-token bucket must overflow.
    with ThreadPoolExecutor(max_workers=51) as pool:
        results = list(pool.map(lambda _i: _fire(), range(51)))

    statuses = [s for s, _ in results]
    codes = [c for _, c in results]

    assert 429 in statuses, f"expected at least one 429; got {statuses}"
    rpm_codes = [c for s, c in zip(statuses, codes) if s == 429]
    assert "GW_QUOTA_RPM_EXCEEDED" in rpm_codes, (statuses, codes)


# =========================================================================
# 3. Markup product: user × api-key × model = product
# =========================================================================


def test_permission_merge_markup_product(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    usage_records_consumer: "UsageRecordsConsumer",
) -> None:
    """User markup=1.10, api-key markup=1.20, model markup=1.10.

    Issue one request, read `usage:records`, and verify the
    `markup_chain_snapshot.product` field equals 1.452 (±epsilon).
    """
    prefix = "pm_markup"
    _seed_happy_path(
        fake_web,
        prefix=prefix,
        user_markup=1.10,
        api_key_markup=1.20,
        model_markup=1.10,
    )
    fake_upstream.reset_scenario()

    # Issue one happy-path request.
    client = make_openai_client(gateway_url, f"sk-{prefix}", f"ak-{prefix}")
    stream = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    # Drain the stream.
    for _ in stream:
        pass

    # Read the usage:records entry.
    record = usage_records_consumer.next_record(timeout=5.0)
    while record.get("api_key_id") != f"ak-{prefix}":
        record = usage_records_consumer.next_record(timeout=5.0)
    markup_snap = record.get("markup_chain_snapshot", {})
    product = markup_snap.get("product", 0.0)

    expected_product = 1.10 * 1.20 * 1.10
    assert abs(product - expected_product) < 0.01, (
        f"expected markup product ≈ {expected_product:.3f}, got {product}; "
        f"full snapshot: {markup_snap}"
    )


# =========================================================================
# 4. ReDoS: pathological regex killed within budget
# =========================================================================


@pytest.mark.xfail(
    reason=(
        "model_whitelist_regex field not yet on ApiKeyEntry; "
        "Rust regex crate is RE2-style (linear) so (a+)+$ cannot cause "
        "catastrophic backtracking. Test documents expected behaviour."
    ),
    strict=False,
)
def test_redos_pathological_regex_killed_within_budget(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Api-key with pathological regex ``(a+)+$``, request model
    ``aaaaaaaaaaaaaaaaab``. Permission denied, ``gw_redos_timeout_total``
    incremented, response latency < 200ms.

    NOTE: The `model_whitelist_regex` field does not exist on `ApiKeyEntry`
    yet. This test seeds it via raw `put_entity` to document the expected
    integration point. The gateway's serde will likely ignore the unknown
    field, so the model permission check falls through to `allowed_models`
    (empty = unrestricted) and the request succeeds instead of being denied.
    When the field lands, remove the `xfail` marker.
    """
    prefix = "rd_budget"
    uid = f"u-{prefix}"
    akid = f"ak-{prefix}"
    ak_plain = f"sk-{prefix}"
    vid = f"v-{prefix}"
    vkid = f"vk-{prefix}"
    grp = f"grp-{prefix}"

    from conftest import sha256_bytes, encrypt_upstream_key

    # Seed user.
    fake_web.put_entity("users", uid, {
        "user_id": uid,
        "parent_id": None,
        "tier": "standard",
        "status": "active",
        "allowed_models": [],
        "allowed_channels": [],
        "allowed_groups": [grp],
        "rpm_limit": 1000,
        "tpm_limit": 0,
        "max_concurrent_streams": 100,
        "daily_quota_tokens": 0,
        "user_group_ids": [],
        "markup": 1.0,
        "debug_enabled": False,
    })
    fake_web.set_balance(uid, BALANCE_LARGE)

    # Seed api-key WITH model_whitelist_regex (not yet on ApiKeyEntry).
    fake_web.put_entity("api_keys", akid, {
        "key_id": akid,
        "user_id": uid,
        "status": "active",
        "hash": sha256_bytes(ak_plain),
        "allowed_models": [],
        "allowed_channels": [],
        "rpm_limit": 0,
        "tpm_limit": 0,
        "markup": 1.0,
        "ip_allowlist": None,
        "expires_at": None,
        "signature_required": False,
        "signature_algo": "hmac_sha256",
        "force_capabilities": [],
        "match_mode_exclusive": False,
        "model_whitelist_regex": "(a+)+$",
    })

    # Seed upstream infra for the model.
    pathological_model = "aaaaaaaaaaaaaaaaab"
    seed_vendor(fake_web, vendor_id=vid)
    seed_vendor_key(
        fake_web,
        key_id=vkid,
        vendor_id=vid,
        model=pathological_model,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_model_group(fake_web, group=grp, vendor_key_ids=[vkid])
    seed_pricing(fake_web, model=pathological_model)
    fake_upstream.reset_scenario()

    # Snapshot metric BEFORE.
    pre = metrics_snapshot("gw_redos_timeout_total", metrics_url)

    body = {
        "model": pathological_model,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": True,
    }

    t0 = time.monotonic()
    resp = _post(gateway_url, headers=_pair(ak_plain, akid), body=body)
    elapsed_ms = (time.monotonic() - t0) * 1000

    # Expected: denied (403) because regex timed out → model match None.
    assert resp.status_code == 403, (
        f"expected 403, got {resp.status_code}; body={resp.text[:300]}"
    )

    # Latency must be < 200ms (budget is 100ms + overhead).
    assert elapsed_ms < 200, f"response took {elapsed_ms:.0f}ms, budget is 100ms"

    # Metric must have incremented.
    post = metrics_snapshot("gw_redos_timeout_total", metrics_url)
    lbl = f'api_key_id="{akid}"'
    delta = label_delta(pre, post, lbl)
    assert delta >= 1, f"gw_redos_timeout_total delta={delta}, expected ≥1"


# =========================================================================
# 5. ReDoS: auto-disable after TIMEOUT_THRESHOLD (10)
# =========================================================================


@pytest.mark.xfail(
    reason=(
        "model_whitelist_regex field not yet on ApiKeyEntry; "
        "Rust regex crate is RE2-style (linear) so (a+)+$ cannot cause "
        "catastrophic backtracking. Test documents expected behaviour."
    ),
    strict=False,
)
def test_redos_auto_disable_after_threshold(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Fire 11 requests with pathological regex (TIMEOUT_THRESHOLD=10).
    After 10 timeouts the api-key's regex evaluator is auto-disabled.
    The 11th request is denied EVEN for a non-pathological model name that
    would normally match.

    Same caveat as test 4 — `model_whitelist_regex` is not yet wired.
    """
    prefix = "rd_disable"
    uid = f"u-{prefix}"
    akid = f"ak-{prefix}"
    ak_plain = f"sk-{prefix}"
    vid = f"v-{prefix}"
    vkid = f"vk-{prefix}"
    grp = f"grp-{prefix}"

    from conftest import sha256_bytes

    # Seed user.
    fake_web.put_entity("users", uid, {
        "user_id": uid,
        "parent_id": None,
        "tier": "standard",
        "status": "active",
        "allowed_models": [],
        "allowed_channels": [],
        "allowed_groups": [grp],
        "rpm_limit": 1000,
        "tpm_limit": 0,
        "max_concurrent_streams": 100,
        "daily_quota_tokens": 0,
        "user_group_ids": [],
        "markup": 1.0,
        "debug_enabled": False,
    })
    fake_web.set_balance(uid, BALANCE_LARGE)

    # Seed api-key with pathological regex.
    fake_web.put_entity("api_keys", akid, {
        "key_id": akid,
        "user_id": uid,
        "status": "active",
        "hash": sha256_bytes(ak_plain),
        "allowed_models": [],
        "allowed_channels": [],
        "rpm_limit": 0,
        "tpm_limit": 0,
        "markup": 1.0,
        "ip_allowlist": None,
        "expires_at": None,
        "signature_required": False,
        "signature_algo": "hmac_sha256",
        "force_capabilities": [],
        "match_mode_exclusive": False,
        "model_whitelist_regex": "(a+)+$",
    })

    # Seed upstream infra for both pathological and normal models.
    pathological_model = "aaaaaaaaaaaaaaaaab"
    normal_model = "gpt-4"
    seed_vendor(fake_web, vendor_id=vid)
    # Vendor key that covers both models.
    seed_vendor_key(
        fake_web,
        key_id=vkid,
        vendor_id=vid,
        model=normal_model,
        accessible_endpoints=[PROTO_OPENAI],
        accessible_models=[pathological_model, normal_model],
    )
    seed_model_group(fake_web, group=grp, vendor_key_ids=[vkid])
    seed_pricing(fake_web, model=pathological_model)
    seed_pricing(fake_web, model=normal_model)
    fake_upstream.reset_scenario()

    # Snapshot metric BEFORE.
    pre = metrics_snapshot("gw_redos_timeout_total", metrics_url)

    headers = _pair(ak_plain, akid)

    # Fire 11 requests with the pathological model to accumulate timeouts.
    pathological_body = {
        "model": pathological_model,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": True,
    }
    for _ in range(11):
        _post(gateway_url, headers=headers, body=pathological_body)

    # Now try a normal model name that would trivially match any sane regex.
    # After auto-disable, even this should be denied.
    normal_body = {
        "model": normal_model,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": True,
    }
    resp = _post(gateway_url, headers=headers, body=normal_body)

    # After auto-disable, the api-key's regex evaluator returns None
    # → permission denied (403).
    assert resp.status_code == 403, (
        f"expected 403 (auto-disabled), got {resp.status_code}; "
        f"body={resp.text[:300]}"
    )

    # Metric must show >= 10 timeouts for this api-key.
    post = metrics_snapshot("gw_redos_timeout_total", metrics_url)
    lbl = f'api_key_id="{akid}"'
    delta = label_delta(pre, post, lbl)
    assert delta >= 10, (
        f"gw_redos_timeout_total delta={delta}, expected ≥10"
    )
