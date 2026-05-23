"""B2 docker e2e: `/healthz/*` management-token gate (6 paths).

Exercises the full B2 verify chain end-to-end against a live gateway brought
up by `make e2e-up`. Path through the gateway:

    parse_mgmt_authorization     (gw-auth::mgmt — splits "Mgmt <token>")
    -> split_mgmt_token_payload  (mt_<id>.<secret> split, prefix check)
    -> EntityStore::mgmt_token_or_load  (Moka L0 -> Redis L1 -> PG L2)
    -> RedisJwtBlacklist::check_mgmt    (FailClosed — Redis error => Revoked)
    -> verify_mgmt_token_secret(scope=AdminRead)  (constant-time hash + status
        + expiry + scope cover)

Six scenarios mirror the `MgmtVerifyOutcome` enum + the unauthenticated
`/livez` surface and the front-door MISSING branch:

  1. test_healthz_without_token_returns_401     -> GW_AUTH_MGMT_TOKEN_MISSING
  2. test_healthz_with_admin_read_token_returns_200  (Ok)
  3. test_healthz_with_user_self_scope_returns_403 -> GW_AUTH_MGMT_TOKEN_SCOPE_DENIED
  4. test_healthz_with_expired_token_returns_401  -> GW_AUTH_MGMT_TOKEN_EXPIRED
  5. test_healthz_after_blacklist_returns_401     -> GW_AUTH_MGMT_TOKEN_REVOKED
  6. test_livez_still_no_auth                     (/livez is intentionally unauth)

The `Disabled` branch is intentionally left to a unit test (gw-auth::mgmt
already covers it); covering it here would only re-exercise the same code
path as `Expired`.

Wire conventions reused from conftest:
- `gateway_health_url` -> http://localhost:8080 (GW_SERVER__HEALTH_PORT).
- `seed_mgmt_token(token_id, secret, scope, expires_in_secs)` writes the row
   through fake_web; the upsert XADDs an `events:entity` invalidation so a
   re-seed of the same id refreshes the gateway's Moka cache.
- `mgmt_blacklist_set(token_id, ttl_secs)` SETs `mgmt_token:blacklist:<id>`
   via fake_web; the gateway's `check_mgmt` probes that key per-request.

IDs are unique per test (mt_r1, mt_u1, mt_e1, mt_b1) so the Moka cache
in the long-lived gateway container can't serve a stale row from a
previous test — same rationale as test_auth_failures.py.
"""
from __future__ import annotations

import requests


def test_healthz_without_token_returns_401(gateway_health_url: str) -> None:
    """B2 front-door: no Authorization header -> 401 MISSING.

    `parse_mgmt_authorization` returns None, which `verify_healthz_request`
    maps to `HealthzAuthError::Missing`. Body is the bare error code
    string (text/plain), not a JSON envelope — the health surface is a
    hand-rolled HTTP/1.1 responder, not the codec.
    """
    resp = requests.get(
        f"{gateway_health_url}/healthz/circuits", timeout=5
    )
    assert resp.status_code == 401, resp.text
    assert b"GW_AUTH_MGMT_TOKEN_MISSING" in resp.content


def test_healthz_with_admin_read_token_returns_200(
    gateway_health_url: str, seed_mgmt_token
) -> None:
    """Ok path: AdminRead scope covers AdminRead-required `/healthz/circuits`."""
    seed_mgmt_token("mt_r1", "secret_r1", scope="admin_read")
    resp = requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_r1.secret_r1"},
        timeout=5,
    )
    assert resp.status_code == 200, resp.text
    # The body is the snapshot JSON emitted by `KeyHealthStore::snapshot_json`.
    # Loose assertion — Content-Type or a JSON-looking prefix is enough; we
    # only care that the verify chain returned an OK envelope here, not the
    # exact snapshot shape (that is covered by gw-health unit tests).
    assert resp.headers.get(
        "content-type", ""
    ).startswith("application/json") or resp.text.lstrip().startswith(
        ("{", "[")
    )


def test_healthz_with_user_self_scope_returns_403(
    gateway_health_url: str, seed_mgmt_token
) -> None:
    """ScopeDenied: `user_self` does NOT cover `admin_read` per
    `MgmtScope::covers`. 403 (Forbidden) is the only non-401 branch."""
    seed_mgmt_token("mt_u1", "secret_u1", scope="user_self")
    resp = requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_u1.secret_u1"},
        timeout=5,
    )
    assert resp.status_code == 403, resp.text
    assert b"GW_AUTH_MGMT_TOKEN_SCOPE_DENIED" in resp.content


def test_healthz_with_expired_token_returns_401(
    gateway_health_url: str, seed_mgmt_token
) -> None:
    """Expired: `expires_at` in the past -> `MgmtVerifyOutcome::Expired` ->
    401 GW_AUTH_MGMT_TOKEN_EXPIRED."""
    seed_mgmt_token(
        "mt_e1", "secret_e1", scope="admin_read", expires_in_secs=-1
    )
    resp = requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_e1.secret_e1"},
        timeout=5,
    )
    assert resp.status_code == 401, resp.text
    assert b"GW_AUTH_MGMT_TOKEN_EXPIRED" in resp.content


def test_healthz_after_blacklist_returns_401(
    gateway_health_url: str, seed_mgmt_token, mgmt_blacklist_set
) -> None:
    """Revoked: post-cache blacklist SET takes effect IMMEDIATELY (the
    blacklist probe is independent of EntityStore's Moka cache).

    Sequence:
      1. Seed mt_b1 (admin_full scope) -> first request 200.
      2. SET mgmt_token:blacklist:mt_b1 via fake_web -> next request 401.
    """
    seed_mgmt_token("mt_b1", "secret_b1", scope="admin_full")

    # 1. First call succeeds — confirms the token cold-loaded cleanly.
    first = requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_b1.secret_b1"},
        timeout=5,
    )
    assert first.status_code == 200, first.text

    # 2. Simulate the Go-side revoke surface.
    mgmt_blacklist_set("mt_b1", ttl_secs=60)

    # 3. Same headers, but `check_mgmt` now flags the token as blacklisted.
    second = requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_b1.secret_b1"},
        timeout=5,
    )
    assert second.status_code == 401, second.text
    assert b"GW_AUTH_MGMT_TOKEN_REVOKED" in second.content


def test_livez_still_no_auth(gateway_health_url: str) -> None:
    """`/livez` is deliberately OUTSIDE the mgmt-verify chain (the
    orchestrator hits it pre-bootstrap, before any token plumbing).

    Regression guard against accidentally extending the verify branch to
    cover the liveness endpoint.
    """
    resp = requests.get(f"{gateway_health_url}/livez", timeout=5)
    assert resp.status_code == 200, resp.text
    assert resp.text.strip() == "ok"
