"""E2E pytest harness fixtures.

This module is the SKELETON for the black-box scenario suite. It exposes:

- session-scoped URL fixtures resolved from env (with sensible localhost
  defaults so a dev can `pytest` against a docker-compose stack);
- `FakeWebClient` — a thin `requests` wrapper around the fake control-plane
  admin API (`crates/e2e-fake-web`). Method shapes mirror the real routes
  one-for-one;
- `FakeUpstreamClient` — a thin wrapper around the fake upstream scenario
  admin API (`crates/e2e-fake-upstream`). Scenario DSL is documented in
  that crate's README;
- `isolate_state` — an autouse function-scoped fixture that resets both
  fakes BEFORE every test, guaranteeing per-test isolation regardless of
  ordering.

The gateway SDK client constructors (openai / anthropic / google-genai)
are intentionally NOT defined here — that wiring is E3-B's job, when the
scenario files start hitting a live gateway with pair-header auth.
"""

from __future__ import annotations

import os
from typing import Any, Optional

import pytest
import requests


# ---------------------------------------------------------------------------
# URL fixtures (session-scoped — these are stable for the whole pytest run).
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def gateway_url() -> str:
    """Base URL of the gateway under test (business port)."""
    return os.environ.get("GATEWAY_URL", "http://localhost:8000")


@pytest.fixture(scope="session")
def fake_web_url() -> str:
    """Base URL of the fake control-plane admin API."""
    return os.environ.get("FAKE_WEB_URL", "http://localhost:7000")


@pytest.fixture(scope="session")
def fake_upstream_admin_url() -> str:
    """Base URL of the fake upstream's scenario admin API (NOT the
    business port that the gateway forwards traffic to)."""
    return os.environ.get("FAKE_UPSTREAM_ADMIN_URL", "http://localhost:9001")


# ---------------------------------------------------------------------------
# FakeWebClient — admin client for the control-plane fake.
# ---------------------------------------------------------------------------


class FakeWebClient:
    """Thin wrapper around fake_web's admin HTTP API.

    Methods correspond one-for-one to the routes registered in
    `crates/e2e-fake-web/src/lib.rs::router`. Every call raises on a
    non-2xx response so test failures point at the unexpected admin
    failure rather than at a downstream assertion.
    """

    def __init__(self, base: str) -> None:
        # Strip a trailing slash so f"{base}/foo" never produces "//foo".
        self.base = base.rstrip("/")
        # Reusing a Session across the per-test isolate_state.reset()
        # autouse keeps the TCP connection warm, which is a noticeable
        # speedup over module-level `requests.post(...)` on every call.
        self.session = requests.Session()

    # ----- entity rows -----------------------------------------------------

    def put_entity(self, table: str, id: str, payload: dict) -> None:
        r = self.session.post(
            f"{self.base}/entity/{table}/{id}", json=payload, timeout=10
        )
        r.raise_for_status()

    def delete_entity(self, table: str, id: str) -> None:
        r = self.session.delete(f"{self.base}/entity/{table}/{id}", timeout=10)
        r.raise_for_status()

    # ----- counters --------------------------------------------------------

    def set_counter(self, scope: str, id: str, field: str, value: int) -> None:
        r = self.session.post(
            f"{self.base}/counter/{scope}/{id}/{field}",
            json={"value": value},
            timeout=10,
        )
        r.raise_for_status()

    def set_raw_counter(self, key: str, value: int) -> None:
        r = self.session.post(
            f"{self.base}/counter-raw",
            json={"key": key, "value": value},
            timeout=10,
        )
        r.raise_for_status()

    def set_balance(self, user_id: str, micros: int) -> None:
        r = self.session.post(
            f"{self.base}/balance/{user_id}",
            json={"micros": micros},
            timeout=10,
        )
        r.raise_for_status()

    # ----- JWKS / usage / reset -------------------------------------------

    def rotate_jwks(self, kid: str) -> str:
        """Rotate the active JWKS kid and return the new RSA private key
        in PEM form (so scenario code can sign JWTs against it)."""
        r = self.session.post(
            f"{self.base}/jwks/rotate", json={"kid": kid}, timeout=10
        )
        r.raise_for_status()
        return r.json()["private_pem"]

    def usage_stats(self) -> dict:
        """Return {records_xlen, dlq_xlen, detailed_xlen}."""
        r = self.session.get(f"{self.base}/usage/stats", timeout=10)
        r.raise_for_status()
        return r.json()

    def force_drain(self) -> int:
        """Trigger one drain cycle synchronously, return entries processed."""
        r = self.session.post(f"{self.base}/usage/drain", timeout=30)
        r.raise_for_status()
        return r.json()["processed"]

    def reset(self) -> None:
        """Wipe all state (Redis FLUSHALL + TRUNCATE entity tables + drain
        cursor back to 0-0). Used by the per-test isolation fixture."""
        r = self.session.post(f"{self.base}/admin/reset", timeout=30)
        r.raise_for_status()

    # ----- convenience wrappers (thin sugar over put_entity) ---------------

    def put_api_key(self, id: str, **payload: Any) -> None:
        self.put_entity("api_keys", id, payload)

    def put_vendor(self, id: str, **payload: Any) -> None:
        self.put_entity("vendors", id, payload)

    def put_vendor_key(self, id: str, **payload: Any) -> None:
        self.put_entity("vendor_keys", id, payload)

    def put_model_group(self, id: str, **payload: Any) -> None:
        self.put_entity("model_groups", id, payload)

    def put_user(self, id: str, **payload: Any) -> None:
        self.put_entity("users", id, payload)

    def put_auth_config(self, payload: dict) -> None:
        # There is exactly one auth_configs row, always id "global".
        self.put_entity("auth_configs", "global", payload)


@pytest.fixture
def fake_web(fake_web_url: str) -> FakeWebClient:
    return FakeWebClient(fake_web_url)


# ---------------------------------------------------------------------------
# FakeUpstreamClient — scenario admin for the fake upstream.
# ---------------------------------------------------------------------------


# Baseline scenario the reset fixture posts to fake_upstream. Matches the
# fake_upstream binary's built-in default (one route, SSE with "hi" + DONE)
# but pushing it explicitly guarantees a clean state regardless of what the
# previous test installed.
_DEFAULT_SCENARIO: dict = {
    "routes": [
        {
            "match": {
                "method": "POST",
                "path_regex": "^/v1/chat/completions$",
            },
            "response": {
                "kind": "sse",
                "delay_ms": 0,
                "events": [
                    {
                        "data": '{"choices":[{"delta":{"content":"hi"}}]}',
                    },
                    {"data": "[DONE]"},
                ],
            },
        }
    ]
}


class FakeUpstreamClient:
    """Thin wrapper around fake_upstream's scenario admin API.

    The scenario DSL is:

        {
          "routes": [
            {
              "match": {"method"?, "path_regex"?, "header_eq"? [name,value]},
              "auth_check"?: {"header": str, "must_start_with": str},
              "response": {"kind": "...", ...}
            }
          ]
        }

    Response kinds: `static`, `sse`, `tcp_reset_after_headers`,
    `tcp_reset_mid_body{after_bytes}`, `connection_refused_first_n{n}`,
    `slow_body_byte_per_second{body,bps}`, `partial_sse_event{half_body}`,
    `status_with_retry_after{status,retry_after_secs,body}`.
    """

    def __init__(self, admin_base: str) -> None:
        self.admin = admin_base.rstrip("/")
        # Reuse a single Session — same rationale as `FakeWebClient`.
        self.session = requests.Session()

    def set_scenario(self, scenario: dict) -> None:
        r = self.session.post(
            f"{self.admin}/admin/scenario", json=scenario, timeout=10
        )
        r.raise_for_status()

    def reset_scenario(self) -> None:
        """Restore the known-good baseline scenario (`POST /v1/chat/completions`
        -> SSE with 'hi' + DONE)."""
        self.set_scenario(_DEFAULT_SCENARIO)

    def healthz(self) -> bool:
        try:
            r = self.session.get(f"{self.admin}/healthz", timeout=5)
        except requests.RequestException:
            return False
        return r.ok and r.text.strip() == "ok"

    # ----- helper builders --------------------------------------------------

    @staticmethod
    def sse_route(
        path_regex: str,
        events: list[dict],
        auth_check: Optional[dict] = None,
        method: str = "POST",
        delay_ms: int = 0,
    ) -> dict:
        """Build a single scenario route returning an SSE stream.

        `events` is a list of `{"data": str, "after_ms"?: int}`. Pass
        `auth_check={"header": "...", "must_start_with": "..."}` to make
        the route 401 on a missing/wrong header.
        """
        route: dict = {
            "match": {"method": method, "path_regex": path_regex},
            "response": {"kind": "sse", "delay_ms": delay_ms, "events": events},
        }
        if auth_check is not None:
            route["auth_check"] = auth_check
        return route


@pytest.fixture
def fake_upstream(fake_upstream_admin_url: str) -> FakeUpstreamClient:
    return FakeUpstreamClient(fake_upstream_admin_url)


# ---------------------------------------------------------------------------
# _check_fakes_alive — session-scoped autouse probe that fails fast if either
# fake is unreachable, with a clearer message than the per-test reset would
# produce. Session-scoped fixtures sort before function-scoped ones, so this
# runs before `isolate_state` automatically. Collection-only runs
# (`pytest --collect-only`) do NOT execute fixtures, so this stays inert
# there.
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session", autouse=True)
def _check_fakes_alive(fake_web_url: str, fake_upstream_admin_url: str) -> None:
    for name, url in [
        ("fake_web", f"{fake_web_url}/healthz"),
        ("fake_upstream admin", f"{fake_upstream_admin_url}/healthz"),
    ]:
        try:
            requests.get(url, timeout=2).raise_for_status()
        except Exception as e:
            pytest.exit(
                f"{name} not reachable at {url}: {e}. Is the e2e stack up? (`make e2e-up`)",
                returncode=2,
            )


# ---------------------------------------------------------------------------
# isolate_state — autouse fixture that resets both fakes before every test.
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def isolate_state(
    fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient
):
    """Reset all e2e fakes BEFORE each test.

    Order: fake_web first (wipes PG + Redis + drain cursor), then
    fake_upstream (restores the baseline scenario). We do this BEFORE the
    test rather than after so that:

    - a failing test leaves dirty state visible for post-mortem;
    - a test that doesn't depend on this fixture explicitly still runs
      against a clean slate.
    """
    fake_web.reset()
    fake_upstream.reset_scenario()
    yield
