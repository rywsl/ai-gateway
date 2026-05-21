"""E4-A: auth-failure black-box scenarios (8).

Each scenario seeds the minimal entity graph, drives a request straight at the
gateway business port with raw `requests` (full header control — the vendor
SDKs will not let us omit/forge the auth + signature headers these tests need),
and asserts on the HTTP status + the gateway's error envelope.

Gateway error envelope (verified — crates/gw-codec/src/openai/codec.rs
::render_error): the gateway emits an OpenAI-style body

    {"error": {"type": <category>, "message": <msg>, "code": "GW_AUTH_*"}}

so the GW_AUTH_* discriminant lands at `body["error"]["code"]`. We POST every
scenario to `/v1/chat/completions`, which selects the OpenAI client codec, so
all 8 share this envelope shape.

Auth-step ordering inside request_filter (crates/gw-proxy/src/filters/request.rs):
  1. Bearer-JWT vs (x-api-key, x-api-key-id) pair branch
  2. (pair) api-key verify -> expiry check  (GW_AUTH_KEY_EXPIRED)
  3. IP blocklist, then per-key IP allowlist (GW_AUTH_KEY_IP_NOT_ALLOWED)
  4. body read -> HMAC verify               (GW_AUTH_SIGNATURE_{MISSING,INVALID,TIMESTAMP_DRIFT})
  5. codec parse, permissions, quota, ...
So each negative scenario rejects at its own step before any later step runs.

UNIQUE IDS PER TEST — why it matters
------------------------------------
The fake_web `reset()` (autouse `isolate_state`) does FLUSHALL + TRUNCATE but
*deliberately skips* emitting cache-invalidation events (the Redis stream that
carries them is wiped in the same reset). The gateway is a long-lived container
whose Moka entity caches therefore SURVIVE the reset. The harness contract is
that the next test re-populates whatever ids it touches via `put_entity` (which
emits an invalidation) — but that invalidation is processed by an ASYNC stream
consumer, so a request firing microseconds after the put can still observe the
stale cached row (this bit the first draft: a later test's `ip_allowlist`
api-key leaked into earlier tests reusing the same id `ak1`).

The robust fix used here: every test uses its OWN api-key id / user id. A
brand-new id has no cache entry, so the gateway cold-loads it straight from PG
(which the test just wrote) — race-free, no dependence on invalidation timing.

The global AuthConfig (expected_iss/aud + HS256 secret) is seeded into postgres
at initdb time (tests/e2e/seed_auth_config.sql) so the gateway's boot-built
JwtValidator enforces JWT_ISS / JWT_AUD for the whole suite. The per-test reset
TRUNCATEs auth_configs, but the validator is held in memory from boot, so
enforcement persists regardless — expected.
"""

from __future__ import annotations

import time

import jwt as pyjwt
import requests

from conftest import (
    FakeWebClient,
    HS256_SECRET,
    JWT_AUD,
    JWT_ISS,
    seed_model_group,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
)

PROTO_OPENAI = "open_ai"
MODEL = "gpt-4"

# `ApiKeyEntry.expires_at` is `Option<time::OffsetDateTime>` and the workspace
# builds `time` with only the `serde` feature (NOT `serde-human-readable`), so
# its default Deserialize uses `deserialize_tuple(9)` — it expects a 9-element
# component array, NOT an ISO8601 string. An ISO string fails the cold-load with
# `invalid type: string ..., expected an OffsetDateTime`, which surfaces to the
# client as GW_AUTH_INVALID_APIKEY (the api-key cold-load returned None), NOT
# the GW_AUTH_KEY_EXPIRED we want. The tuple layout (from time's serde impl,
# OffsetDateTime serialize = (year, ordinal, hour, min, sec, nano, off_h, off_m,
# off_s)) for 2000-01-01T00:00:00Z is:
PAST_EXPIRES_AT = [2000, 1, 0, 0, 0, 0, 0, 0, 0]  # year, ordinal(day-of-year), h, m, s, ns, +offset h/m/s

# A minimal, valid OpenAI chat-completions body with stream=true. For the
# negative scenarios auth rejects before the codec ever parses this; for the
# JWKS positive leg it must parse cleanly so the request can reach upstream.
_CHAT_BODY = {
    "model": MODEL,
    "messages": [{"role": "user", "content": "hi"}],
    "stream": True,
}


# ---------------------------------------------------------------------------
# Seeding helpers.
# ---------------------------------------------------------------------------


def _seed_routable_graph(
    fake_web: FakeWebClient,
    *,
    user_id: str,
    group: str,
    vendor_id: str,
    vendor_key_id: str,
) -> None:
    """Seed everything DOWNSTREAM of the api-key (vendor + vendor_key +
    model_group + pricing + balance) so that, once auth passes, the request is
    fully routable. The api-key/user rows themselves are seeded by the caller
    (each test flexes a different api-key knob)."""
    seed_vendor(fake_web, vendor_id=vendor_id)
    seed_vendor_key(
        fake_web,
        key_id=vendor_key_id,
        vendor_id=vendor_id,
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_model_group(
        fake_web, group=group, vendor_key_ids=[vendor_key_id], strategy="fixed_order"
    )
    seed_pricing(fake_web, model=MODEL)
    fake_web.set_balance(user_id, 1_000_000_000)


def _post(
    gateway_url: str, *, headers: dict, body: dict | None = None
) -> requests.Response:
    """Raw POST to the gateway's chat-completions surface. We read the whole
    response; for these tests we only care about the status line + the error
    envelope, which arrives before/instead of any stream frames."""
    return requests.post(
        f"{gateway_url.rstrip('/')}/v1/chat/completions",
        json=body if body is not None else _CHAT_BODY,
        headers=headers,
        timeout=30,
    )


def _error_code(resp: requests.Response) -> str | None:
    """Pull the GW_AUTH_* code out of the gateway error envelope, or None if the
    body isn't the expected JSON shape (so the caller can fall back to status)."""
    try:
        payload = resp.json()
    except ValueError:
        return None
    err = payload.get("error")
    if isinstance(err, dict):
        return err.get("code")
    return None


def _pair_headers(api_key: str, key_id: str, **extra: str) -> dict:
    h = {
        "x-api-key": api_key,
        "x-api-key-id": key_id,
        "content-type": "application/json",
    }
    h.update(extra)
    return h


# ---------------------------------------------------------------------------
# 1. Expired API key -> 401 GW_AUTH_KEY_EXPIRED
# ---------------------------------------------------------------------------


def test_expired_api_key_rejects_401(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    # Routable downstream graph so the ONLY rejection reason is the expiry
    # (request.rs step 2, before any routing/permission work).
    _seed_routable_graph(
        fake_web,
        user_id="u-exp",
        group="grp-exp",
        vendor_id="v-exp",
        vendor_key_id="vk-exp",
    )
    seed_user_apikey(
        fake_web,
        user_id="u-exp",
        api_key_id="ak-exp",
        api_key_plaintext="sk-exp",
        allowed_groups=["grp-exp"],
        expires_at=PAST_EXPIRES_AT,  # past -> closed-interval reject (tuple form)
    )

    resp = _post(gateway_url, headers=_pair_headers("sk-exp", "ak-exp"))
    assert resp.status_code == 401, resp.text
    assert _error_code(resp) == "GW_AUTH_KEY_EXPIRED", resp.text


# ---------------------------------------------------------------------------
# 2. Client IP not in api-key allowlist -> 403 GW_AUTH_KEY_IP_NOT_ALLOWED
# ---------------------------------------------------------------------------


def test_ip_not_allowlisted_rejects_403(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    # 203.0.113.0/24 is TEST-NET-3 (RFC 5737) — the docker-bridge source IP the
    # gateway sees can never fall inside it, so the allowlist always rejects.
    seed_user_apikey(
        fake_web,
        user_id="u-ip",
        api_key_id="ak-ip",
        api_key_plaintext="sk-ip",
        allowed_groups=["grp-ip"],
        ip_allowlist=["203.0.113.0/24"],
    )
    fake_web.set_balance("u-ip", 1_000_000_000)

    resp = _post(gateway_url, headers=_pair_headers("sk-ip", "ak-ip"))
    assert resp.status_code == 403, resp.text
    assert _error_code(resp) == "GW_AUTH_KEY_IP_NOT_ALLOWED", resp.text


# ---------------------------------------------------------------------------
# 3-5. HMAC. signature_required=True; no ip_allowlist so the HMAC step (4) is
# the first to reject. Each uses a distinct api-key id (cache isolation).
# ---------------------------------------------------------------------------


def _seed_hmac_key(fake_web: FakeWebClient, *, suffix: str) -> tuple[str, str]:
    user_id = f"u-sig-{suffix}"
    api_key_id = f"ak-sig-{suffix}"
    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=f"sk-sig-{suffix}",
        allowed_groups=[f"grp-sig-{suffix}"],
        signature_required=True,
        signature_algo="hmac_sha256",
    )
    fake_web.set_balance(user_id, 1_000_000_000)
    return api_key_id, f"sk-sig-{suffix}"


def test_hmac_required_missing_rejects_401(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    key_id, plaintext = _seed_hmac_key(fake_web, suffix="missing")
    # No x-signature / x-timestamp at all.
    resp = _post(gateway_url, headers=_pair_headers(plaintext, key_id))
    assert resp.status_code == 401, resp.text
    assert _error_code(resp) == "GW_AUTH_SIGNATURE_MISSING", resp.text


def test_hmac_bad_signature_rejects_401(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    key_id, plaintext = _seed_hmac_key(fake_web, suffix="invalid")
    now = int(time.time())
    headers = _pair_headers(
        plaintext, key_id, **{"x-timestamp": str(now), "x-signature": "deadbeef"}
    )
    resp = _post(gateway_url, headers=headers)
    assert resp.status_code == 401, resp.text
    assert _error_code(resp) == "GW_AUTH_SIGNATURE_INVALID", resp.text


def test_hmac_timestamp_drift_rejects_401(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    key_id, plaintext = _seed_hmac_key(fake_web, suffix="drift")
    # 1000s in the past — outside the ±300s window. The drift check runs before
    # the HMAC compare, so the (wrong) signature never matters.
    drifted = int(time.time()) - 1000
    headers = _pair_headers(
        plaintext, key_id, **{"x-timestamp": str(drifted), "x-signature": "deadbeef"}
    )
    resp = _post(gateway_url, headers=headers)
    assert resp.status_code == 401, resp.text
    assert _error_code(resp) == "GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT", resp.text


# ---------------------------------------------------------------------------
# 6. JWT issuer mismatch -> 401 GW_AUTH_JWT_INVALID_ISS
# ---------------------------------------------------------------------------


def test_jwt_iss_mismatch_rejects_401(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    now = int(time.time())
    token = pyjwt.encode(
        {"sub": "u1", "iss": "WRONG-issuer", "aud": JWT_AUD, "exp": now + 3600},
        HS256_SECRET,
        algorithm="HS256",
    )
    resp = _post(
        gateway_url,
        headers={"authorization": f"Bearer {token}", "content-type": "application/json"},
    )
    assert resp.status_code == 401, resp.text
    assert _error_code(resp) == "GW_AUTH_JWT_INVALID_ISS", resp.text


# ---------------------------------------------------------------------------
# 7. JWT audience mismatch -> 401 GW_AUTH_JWT_INVALID_AUD
# ---------------------------------------------------------------------------


def test_jwt_aud_mismatch_rejects_401(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    now = int(time.time())
    token = pyjwt.encode(
        {"sub": "u1", "iss": JWT_ISS, "aud": "WRONG-aud", "exp": now + 3600},
        HS256_SECRET,
        algorithm="HS256",
    )
    resp = _post(
        gateway_url,
        headers={"authorization": f"Bearer {token}", "content-type": "application/json"},
    )
    assert resp.status_code == 401, resp.text
    assert _error_code(resp) == "GW_AUTH_JWT_INVALID_AUD", resp.text


# ---------------------------------------------------------------------------
# 8. JWKS rotation: old kid rejected, new kid accepted.
#
# Exercises per-request JWKS cache invalidation end-to-end:
#
#  1. `rotate_jwks(kid)` writes a fresh RSA keypair into the `jwks` table under
#     `kid` (bumping version) and emits `events:entity {ns="jwks", id=<kid>}`.
#     The gateway's `StreamConsumer` (crates/gw-store/src/stream_consumer.rs)
#     consumes the event and invalidates the cached pubkey for that kid.
#  2. A JWT signed by the FIRST keypair must verify (and the request must pass
#     JWT auth — we don't necessarily require a 200 downstream; any non-JWT-
#     auth-failure status is fine, since this scenario tests JWKS only).
#  3. `rotate_jwks(SAME kid)` replaces the keypair and re-emits the event. A
#     token signed by the OLD priv1 (same kid) must now be REJECTED as
#     GW_AUTH_INVALID_JWT (signature mismatch against the rotated pubkey),
#     and a token signed by priv2 must be accepted.
#
# Invalidation propagates asynchronously through the Redis stream consumer,
# so the assertions poll for up to ~6s. The per-test isolate_state reset
# preserves `events:entity` / `events:counter` (and their gateway-owned
# consumer groups) — without that, the gateway's consume_loop would spin on
# NOGROUP forever after the first reset and rotation would never propagate.
# See `crates/e2e-fake-web/src/store.rs::Store::reset` for why.
#
# We deliberately do NOT seed the full happy-path routable graph for the
# `sub` claim, so a passing JWT lands on quota/routing further downstream;
# we accept any non-JWT-auth-failure status as proof that JWT auth passed.
# ---------------------------------------------------------------------------


def _rs256(priv_pem: str, kid: str, sub: str = "u-jwks") -> str:
    now = int(time.time())
    return pyjwt.encode(
        {"sub": sub, "iss": JWT_ISS, "aud": JWT_AUD, "exp": now + 3600},
        priv_pem,
        algorithm="RS256",
        headers={"kid": kid},
    )


def _bearer(gateway_url: str, token: str) -> requests.Response:
    return _post(
        gateway_url,
        headers={"authorization": f"Bearer {token}", "content-type": "application/json"},
    )


_JWT_AUTH_FAILURE_CODES = {
    "GW_AUTH_INVALID_JWT",
    "GW_AUTH_JWT_INVALID_ISS",
    "GW_AUTH_JWT_INVALID_AUD",
    "GW_AUTH_JWT_NOT_YET_VALID",
    "GW_AUTH_JWT_EXPIRED",
}


def _is_jwt_auth_failure(resp: requests.Response) -> bool:
    return resp.status_code == 401 and _error_code(resp) in _JWT_AUTH_FAILURE_CODES


def _poll(gateway_url, token, *, want_failure: bool, timeout: float = 6.0):
    """Re-send `token` until its JWT-auth disposition matches `want_failure`, or
    timeout. Returns the final response. (Used by the body kept for re-enable.)"""
    deadline = time.monotonic() + timeout
    resp = _bearer(gateway_url, token)
    while time.monotonic() < deadline and _is_jwt_auth_failure(resp) != want_failure:
        time.sleep(0.25)
        resp = _bearer(gateway_url, token)
    return resp


def test_jwks_rotation_old_kid_rejected_new_accepted(
    gateway_url: str, fake_web: FakeWebClient
) -> None:
    # Unique kid per test so the gateway has no cached pubkey on entry —
    # the first rotate_jwks then cold-loads against fresh state, and the
    # SECOND rotation exercises the invalidation path. The Moka cache for
    # `jwks` has a 3600s TTL, so without invalidation the second pubkey
    # would never be observed within the test window.
    kid = f"k-rot-{int(time.time() * 1000)}"

    priv1 = fake_web.rotate_jwks(kid)
    tok1 = _rs256(priv1, kid)
    # We do NOT seed a routable graph for this `sub`, so a passing JWT
    # will land on quota/routing further downstream; any non-JWT-auth
    # failure status is fine — we only care that the JWT itself verified.
    resp1 = _poll(gateway_url, tok1, want_failure=False)
    assert not _is_jwt_auth_failure(resp1), (
        f"token signed by current {kid} key should pass JWT auth; "
        f"got {resp1.status_code} {resp1.text}"
    )

    # Second rotation: SAME kid, new keypair. This emits an
    # `events:entity ns=jwks id=<kid>` invalidation; the gateway's
    # StreamConsumer applies it and the next cold-load picks up priv2's
    # pubkey. Allow up to ~6s for the async stream consumer to process.
    priv2 = fake_web.rotate_jwks(kid)
    tok_old = _rs256(priv1, kid)
    tok_new = _rs256(priv2, kid)

    resp_old = _poll(gateway_url, tok_old, want_failure=True)
    assert _is_jwt_auth_failure(resp_old), (
        f"old-key JWT should be rejected after rotation; "
        f"got {resp_old.status_code} {resp_old.text}"
    )
    assert _error_code(resp_old) == "GW_AUTH_INVALID_JWT", resp_old.text

    resp_new = _poll(gateway_url, tok_new, want_failure=False)
    assert not _is_jwt_auth_failure(resp_new), (
        f"token signed by rotated-in {kid} key should pass JWT auth; "
        f"got {resp_new.status_code} {resp_new.text}"
    )
