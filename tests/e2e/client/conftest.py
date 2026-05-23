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

import hashlib
import os
import re
import time
from typing import Any, Optional

import httpx
import pytest
import requests
from cryptography.hazmat.primitives.ciphers.aead import AESGCM


# ---------------------------------------------------------------------------
# Crypto / encoding helpers (used by E3-B+ scenarios to seed vendor_keys +
# api_keys against the gateway's KEK + sha256 contracts).
#
# The gateway decrypts vendor-key plaintext at load-time using AES-256-GCM
# with the KEK indexed by `kek_kid`. The default KEK_HEX matches the value
# the docker-compose stack injects into the gateway container; override via
# the GW_KEK_HEX env var for one-off testing against a custom-KEK deployment.
# ---------------------------------------------------------------------------


KEK_HEX = os.environ.get("GW_KEK_HEX", "deadbeef" * 8)


# ---------------------------------------------------------------------------
# Generic scenario constants + HTTP helpers used by every scenario file. Lifted
# from the per-file copies in test_auth_failures / test_happy_streaming /
# test_quotas so E4-C/D/E (and future suites) can reuse them.
#
# `BALANCE_LARGE` is comfortably above any freeze estimate the gateway will
# compute for a single test request — using it everywhere keeps positive
# scenarios from accidentally tripping the balance gate.
# `MODEL`/`PROTO_OPENAI` are the happy-path defaults; only the few scenario
# files that exercise non-OpenAI/non-`gpt-4` cases override them.
# ---------------------------------------------------------------------------

BALANCE_LARGE = 1_000_000_000

MODEL = "gpt-4"
PROTO_OPENAI = "open_ai"

# Reuse a single Session across `post_chat` invocations: connection pooling
# halves the per-test wall-time for negative scenarios that just POST + assert.
_HTTP_SESSION = requests.Session()


def post_chat(
    gateway_url: str,
    headers: dict,
    body: dict,
    *,
    timeout: float = 10,
) -> requests.Response:
    """One-shot POST to the gateway's `/v1/chat/completions` surface.

    Lifted from the per-file `_post(...)` duplicates in the scenario files —
    they all hit the same URL with the same shape. Reads the whole response;
    callers that care about a stream open their own SDK client instead.
    """
    return _HTTP_SESSION.post(
        f"{gateway_url.rstrip('/')}/v1/chat/completions",
        json=body,
        headers=headers,
        timeout=timeout,
    )


def pair_headers(api_key: str, key_id: str, **extra: str) -> dict:
    """Build the gateway pair-auth header dict.

    Always includes `content-type: application/json` (every scenario POSTs
    JSON). Extra kwargs are merged in last so a caller can append (or
    override) headers like `x-signature` / `x-timestamp` / `x-require-capability`.
    """
    h = {
        "x-api-key": api_key,
        "x-api-key-id": key_id,
        "content-type": "application/json",
    }
    h.update(extra)
    return h


def _error_code(resp: requests.Response) -> Optional[str]:
    """Pull `body["error"]["code"]` out of the gateway's OpenAI-style error
    envelope (`crates/gw-codec/src/openai/codec.rs::render_error`).

    Returns None when the body is not the expected JSON shape (so the
    caller can fall back to status). Tolerant of non-JSON bodies.
    """
    try:
        payload = resp.json()
    except ValueError:
        return None
    err = payload.get("error") if isinstance(payload, dict) else None
    if isinstance(err, dict):
        return err.get("code")
    return None


def wait_for_records(
    fake_web: "FakeWebClient",
    *,
    min_xlen: int = 1,
    timeout: float = 3.0,
) -> int:
    """Poll `usage_stats().records_xlen` until it reaches `min_xlen`, or
    timeout. Returns the final observed xlen (caller asserts).

    The gateway's logging filter XADDs `usage:records` asynchronously after
    stream completion, so positive scenarios that need to observe the ledger
    must wait. 3s is the same budget the original per-file copies used.
    """
    deadline = time.monotonic() + timeout
    xlen = 0
    while time.monotonic() < deadline:
        xlen = fake_web.usage_stats().get("records_xlen", 0)
        if xlen >= min_xlen:
            return xlen
        time.sleep(0.1)
    return fake_web.usage_stats().get("records_xlen", 0)


# ---------------------------------------------------------------------------
# Prometheus metric helpers. Shared by scenario suites that assert on counter
# deltas (E4-C routing/retry, E4-E reliability/DLQ). Both helpers are
# stateless reads against `/metrics` — no fixture dependency, callers pass
# `metrics_url` explicitly.
# ---------------------------------------------------------------------------


def metrics_snapshot(metric: str, metrics_url: str) -> dict[str, float]:
    """Return `{label_pairs -> value}` for every series of `metric`.

    `label_pairs` is the raw `{a="x",b="y"}` string (or "" when unlabelled).
    Values are parsed as floats from the Prometheus text format so both
    counters and gauges (which may carry fractional values) survive intact.
    """
    text = requests.get(f"{metrics_url.rstrip('/')}/metrics", timeout=5).text
    out: dict[str, float] = {}
    line_re = re.compile(
        rf"^{re.escape(metric)}(?:\{{(?P<lbl>[^}}]*)\}})?\s+(?P<v>[-+0-9eE.]+)$"
    )
    for raw in text.splitlines():
        m = line_re.match(raw)
        if m:
            out[m.group("lbl") or ""] = float(m.group("v"))
    return out


def label_delta(pre: dict[str, float], post: dict[str, float], labels: str) -> float:
    """Return the post-minus-pre delta for one specific `labels` series.

    Missing keys default to 0 so the helper is safe to call before the
    metric has been emitted for that label set at all.
    """
    return post.get(labels, 0) - pre.get(labels, 0)


# ---------------------------------------------------------------------------
# Boot-time AuthConfig constants (E4-A). These MUST match the values seeded
# into postgres by `tests/e2e/seed_auth_config.sql`, which is applied during
# initdb (before the gateway boots and builds its JwtValidator). Scenario code
# signs JWTs against these so iss/aud line up with what the validator enforces.
# ---------------------------------------------------------------------------

HS256_SECRET = "e2e-hs256-secret-key"
JWT_ISS = "e2e-issuer"
JWT_AUD = "e2e-aud"


def sha256_bytes(plaintext: str) -> list[int]:
    """Return the 32 raw bytes of sha256(plaintext) as a list of ints.

    The api_keys row stores `hash` as `Vec<u8>` — serde_json deserializes
    Vec<u8> from a JSON array of integers (NOT base64, NOT hex). This
    helper produces the exact shape `serde_json` accepts.
    """
    return list(hashlib.sha256(plaintext.encode("utf-8")).digest())


def encrypt_upstream_key(
    plaintext: str,
    kek_hex: str = KEK_HEX,
    kek_kid: str = "primary",
) -> dict:
    """Encrypt an upstream vendor key into the gateway's `EncryptedKey` shape.

    AES-256-GCM with a fresh 12-byte nonce, NO associated data. The
    `cryptography` library returns `ciphertext || tag` (last 16 bytes are
    the tag) — the gateway expects them stored separately, so we split.
    All byte vectors are emitted as JSON arrays of ints to satisfy serde's
    `Vec<u8>` deserializer.
    """
    kek_bytes = bytes.fromhex(kek_hex)
    if len(kek_bytes) != 32:
        raise ValueError(
            f"KEK must decode to 32 bytes (got {len(kek_bytes)}); check GW_KEK_HEX"
        )
    nonce = os.urandom(12)
    out = AESGCM(kek_bytes).encrypt(nonce, plaintext.encode("utf-8"), None)
    ciphertext, auth_tag = out[:-16], out[-16:]
    return {
        "ciphertext": list(ciphertext),
        "nonce": list(nonce),
        "auth_tag": list(auth_tag),
        "kek_kid": kek_kid,
    }


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


@pytest.fixture(scope="session")
def metrics_url() -> str:
    """Base URL of the gateway's Prometheus `/metrics` surface."""
    return os.environ.get("METRICS_URL", "http://localhost:9090")


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
# Redis access — T4-A (B1 non-stream) needs to ASSERT on the actual JSON
# payload of `usage:records` entries (not just the XLEN that
# `fake_web.usage_stats()` exposes). The fake-web admin API has no
# record-body read endpoint and we deliberately don't extend it from
# T4-A; instead we expose Redis on the host (`6379:6379` in
# `tests/e2e/docker-compose.yml`) and have the host pytest process read
# `usage:records` directly via the `redis` Python package.
#
# Fixtures are function-scoped so a connection failure surfaces per-test.
# The autouse `isolate_state` DELs the stream before each test, so the
# consumer starts each test reading from an empty stream.
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def redis_url() -> str:
    """Base URL of the Redis instance shared with the gateway. Defaults to
    the host-port mapping in `tests/e2e/docker-compose.yml` (`6379:6379`)."""
    return os.environ.get("REDIS_URL", "redis://localhost:6379")


@pytest.fixture
def redis_client(redis_url: str):
    """Plain `redis.Redis` client. Decodes responses to str so the
    `XRANGE`/`XREAD` field maps are dict[str, str] rather than dict[bytes,
    bytes] — keeps the consumer code branch-free."""
    import redis  # local import: test collection survives if package missing

    client = redis.Redis.from_url(redis_url, decode_responses=True)
    try:
        yield client
    finally:
        try:
            client.close()
        except Exception:
            pass


class UsageRecordsConsumer:
    """Tail-only consumer for the `usage:records` stream.

    On construction, anchors `last_id="0"` so we read forward from the
    start of a (just-DEL'd-by-`isolate_state`) stream. Each
    `next_record(timeout)` does `XREAD BLOCK <ms> COUNT 1`, decodes the
    `payload` field as JSON (the gateway XADD's `payload=<json>` —
    see `crates/gw-ledger/src/flusher.rs::flush`), and advances `last_id`.

    Raises `TimeoutError` if no record lands within `timeout`.
    """

    STREAM = "usage:records"

    def __init__(self, client) -> None:
        self.client = client
        # Anchor at 0 so we observe records written after `isolate_state`
        # cleared the stream. `XREAD BLOCK $` would only return records
        # newer than the call site — `isolate_state` already ran before
        # the consumer was materialised, so 0 is race-free.
        self.last_id = "0"

    def next_record(self, timeout: float = 5.0) -> dict:
        import json as _json

        block_ms = max(1, int(timeout * 1000))
        resp = self.client.xread({self.STREAM: self.last_id}, block=block_ms, count=1)
        if not resp:
            raise TimeoutError(
                f"no `{self.STREAM}` entry within {timeout}s (last_id={self.last_id})"
            )
        # resp shape: [(stream_name, [(entry_id, {field: value}), ...])]
        _, entries = resp[0]
        entry_id, fields = entries[0]
        self.last_id = entry_id
        payload = fields.get("payload")
        if payload is None:
            raise AssertionError(
                f"`{self.STREAM}` entry {entry_id} missing `payload` field: {fields!r}"
            )
        try:
            return _json.loads(payload)
        except ValueError as e:
            raise AssertionError(
                f"`{self.STREAM}` entry {entry_id} payload is not JSON: {payload!r}"
            ) from e


@pytest.fixture
def usage_records_consumer(redis_client) -> UsageRecordsConsumer:
    return UsageRecordsConsumer(redis_client)


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


# ---------------------------------------------------------------------------
# SDK client builders (pair-auth: x-api-key + x-api-key-id, NEVER Bearer).
#
# Gateway auth precedence (crates/gw-proxy/src/filters/request.rs):
#   if Authorization: Bearer present -> JWT path (will FAIL for an api key);
#   else if x-api-key-id AND x-api-key present -> pair api-key auth.
# So every helper here must guarantee NO Authorization header reaches the
# gateway, and MUST send the pair.
# ---------------------------------------------------------------------------


def _pair_auth_request_hook(api_key: str, key_id: str):
    """httpx event hook that strips any Authorization header the SDK may
    have attached and sets the gateway's pair-auth headers in its place."""

    def _hook(request: "httpx.Request") -> None:  # type: ignore[name-defined]
        # delete any flavor of "authorization" the SDK injected
        for h in list(request.headers.keys()):
            if h.lower() == "authorization":
                del request.headers[h]
        request.headers["x-api-key"] = api_key
        request.headers["x-api-key-id"] = key_id

    return _hook


def make_openai_client(gateway_url: str, api_key: str, key_id: str):
    """openai.OpenAI() pointed at the gateway with pair-auth.

    The openai SDK ALWAYS sends `Authorization: Bearer <api_key>`. The
    request-hook below deletes that header and substitutes the gateway's
    pair. `api_key="unused"` keeps the SDK's local validation happy.
    """
    import openai  # local import so test collection works even if SDK missing

    transport = httpx.Client(
        event_hooks={"request": [_pair_auth_request_hook(api_key, key_id)]},
        timeout=30.0,
    )
    return openai.OpenAI(
        base_url=f"{gateway_url.rstrip('/')}/v1",
        api_key="unused",
        http_client=transport,
        max_retries=0,
    )


def make_anthropic_client(gateway_url: str, api_key: str, key_id: str):
    """anthropic.Anthropic() pointed at the gateway with pair-auth.

    The anthropic SDK natively sends `x-api-key: <api_key>` and does NOT
    set Authorization, so we'd ordinarily only need `default_headers` for
    x-api-key-id. To be defensive against future SDK changes (and tool
    polyfills that may inject Authorization), we install the same strip
    hook used for openai.
    """
    import anthropic

    transport = httpx.Client(
        event_hooks={"request": [_pair_auth_request_hook(api_key, key_id)]},
        timeout=30.0,
    )
    return anthropic.Anthropic(
        base_url=gateway_url.rstrip("/"),
        api_key=api_key,
        default_headers={"x-api-key-id": key_id},
        http_client=transport,
        max_retries=0,
    )


def make_gemini_client(gateway_url: str, api_key: str, key_id: str):
    """google-genai Client pointed at the gateway with pair-auth.

    The google-genai SDK sends `x-goog-api-key` by default (Authorization
    is NOT set). The gateway ignores `x-goog-api-key`, so we override
    `http_options.headers` with the pair AND we strip Authorization
    defensively via the SDK's own header dict (it doesn't expose request
    hooks). Per ADR-E3-B-1 in the spec, we have the option of dropping
    the SDK and driving Gemini with raw httpx — but the SDK path is
    preferred for symmetry with the other two scenarios.
    """
    from google import genai
    from google.genai import types

    http_opts = types.HttpOptions(
        base_url=gateway_url.rstrip("/"),
        headers={
            "x-api-key": api_key,
            "x-api-key-id": key_id,
        },
    )
    # api_key kw must be set (SDK validates), but we override headers above.
    return genai.Client(api_key=api_key, http_options=http_opts)


# ---------------------------------------------------------------------------
# Entity-row seeding primitives. Each function writes EXACTLY ONE row to the
# fake control plane in the shape that crates/gw-store/src/loader.rs (and
# crates/gw-core/src/entity.rs) deserialize. They are intentionally composable
# so future scenario suites (E4-C multi-key/non-fixed-strategy, E4-D
# cross-protocol with protocol_conversion=true, E4-A/B negative auth paths)
# can build on them without copy-pasting payloads.
#
# Defaults intentionally mirror the happy-path values that the original
# inline `seed_passthrough` body used, so the byte-for-byte payload shape
# the gateway sees is unchanged.
# ---------------------------------------------------------------------------


def seed_user_apikey(
    fake_web: "FakeWebClient",
    *,
    user_id: str,
    api_key_id: str,
    api_key_plaintext: str,
    allowed_groups: list[str],
    # ---- user-row knobs (E4-A/B will flex these) -------------------------
    parent_id: Optional[str] = None,
    tier: str = "standard",
    user_status: str = "active",
    allowed_models: Optional[list[str]] = None,
    allowed_channels: Optional[list[str]] = None,
    rpm_limit: int = 1000,
    tpm_limit: int = 0,
    max_concurrent_streams: int = 100,
    daily_quota_tokens: int = 0,
    user_group_ids: Optional[list[str]] = None,
    user_markup: float = 1.0,
    debug_enabled: bool = False,
    # ---- api_key-row knobs (E4-A/B negative paths) -----------------------
    api_key_status: str = "active",
    api_key_allowed_models: Optional[list[str]] = None,
    api_key_allowed_channels: Optional[list[str]] = None,
    api_key_rpm_limit: int = 0,
    api_key_tpm_limit: int = 0,
    api_key_markup: float = 1.0,
    ip_allowlist: Optional[list[str]] = None,
    expires_at: Optional[str] = None,
    signature_required: bool = False,
    signature_algo: str = "hmac_sha256",
    force_capabilities: Optional[list[str]] = None,
    match_mode_exclusive: bool = False,
) -> None:
    """Write the `users` and `api_keys` rows for one pair-auth principal.

    All non-defaulted knobs default to the happy-path values; flex the
    expiry / ip_allowlist / signature_required / daily_quota_tokens
    arguments to exercise the E4-A/B negative-path scenarios.
    """
    fake_web.put_entity(
        "users",
        user_id,
        {
            "user_id": user_id,
            "parent_id": parent_id,
            "tier": tier,
            "status": user_status,
            "allowed_models": allowed_models if allowed_models is not None else [],
            "allowed_channels": (
                allowed_channels if allowed_channels is not None else []
            ),
            "allowed_groups": allowed_groups,
            "rpm_limit": rpm_limit,
            "tpm_limit": tpm_limit,
            "max_concurrent_streams": max_concurrent_streams,
            "daily_quota_tokens": daily_quota_tokens,
            "user_group_ids": user_group_ids if user_group_ids is not None else [],
            "markup": user_markup,
            "debug_enabled": debug_enabled,
        },
    )
    fake_web.put_entity(
        "api_keys",
        api_key_id,
        {
            "key_id": api_key_id,
            "user_id": user_id,
            "status": api_key_status,
            "hash": sha256_bytes(api_key_plaintext),
            "allowed_models": (
                api_key_allowed_models if api_key_allowed_models is not None else []
            ),
            "allowed_channels": (
                api_key_allowed_channels
                if api_key_allowed_channels is not None
                else []
            ),
            "rpm_limit": api_key_rpm_limit,
            "tpm_limit": api_key_tpm_limit,
            "markup": api_key_markup,
            "ip_allowlist": ip_allowlist,
            "expires_at": expires_at,
            "signature_required": signature_required,
            "signature_algo": signature_algo,
            "force_capabilities": (
                force_capabilities if force_capabilities is not None else []
            ),
            "match_mode_exclusive": match_mode_exclusive,
        },
    )


def seed_vendor(
    fake_web: "FakeWebClient",
    *,
    vendor_id: str,
    base_urls: Optional[list[str]] = None,
    display_name: Optional[str] = None,
    auth_template: str = "Bearer {api_key}",
    timeout_overrides: Optional[dict] = None,
    default_proxy: Optional[str] = None,
) -> None:
    """Write a single `vendors` row.

    `base_urls` defaults to `["http://fake-upstream:9000"]` (the docker-compose
    in-network DNS name for the fake upstream's business port).
    `display_name` defaults to `vendor_id` to match the happy-path payload.
    """
    fake_web.put_entity(
        "vendors",
        vendor_id,
        {
            "id": vendor_id,
            "display_name": display_name if display_name is not None else vendor_id,
            "base_urls": (
                base_urls if base_urls is not None else ["http://fake-upstream:9000"]
            ),
            "auth_template": auth_template,
            "timeout_overrides": timeout_overrides,
            "default_proxy": default_proxy,
        },
    )


def seed_vendor_key(
    fake_web: "FakeWebClient",
    *,
    key_id: str,
    vendor_id: str,
    model: str,
    accessible_endpoints: list[str],
    upstream_key: str = "sk-up",
    protocol_conversion: bool = False,
    status: str = "active",
    forced_capabilities: Optional[list[str]] = None,
    exclusive_capability_match: bool = False,
    weight: int = 1,
    priority: int = 0,
    accessible_models: Optional[list[str]] = None,
    model_mapping: Optional[dict] = None,
    rpm_limit: int = 0,
    tpm_limit: int = 0,
    max_usage_count: Optional[int] = None,
    proxy: Optional[str] = None,
    ip_allowlist: Optional[list[str]] = None,
    expires_at: Optional[str] = None,
    tags: Optional[list[str]] = None,
) -> None:
    """Write a single `vendor_keys` row.

    `accessible_endpoints` is REQUIRED — callers pass `["open_ai"]`,
    `["anthropic"]`, `["gemini"]`, or (for E4-D cross-protocol) any
    combination thereof. `accessible_models` defaults to `[model]` to
    match the happy-path payload; pass an explicit list to widen.
    """
    fake_web.put_entity(
        "vendor_keys",
        key_id,
        {
            "key_id": key_id,
            "vendor_id": vendor_id,
            "api_key_enc": encrypt_upstream_key(upstream_key),
            "accessible_models": (
                accessible_models if accessible_models is not None else [model]
            ),
            "model_mapping": model_mapping if model_mapping is not None else {},
            "accessible_endpoints": accessible_endpoints,
            "protocol_conversion": protocol_conversion,
            "priority": priority,
            "weight": weight,
            "rpm_limit": rpm_limit,
            "tpm_limit": tpm_limit,
            "max_usage_count": max_usage_count,
            "proxy": proxy,
            "ip_allowlist": ip_allowlist,
            "expires_at": expires_at,
            "status": status,
            "tags": tags if tags is not None else [],
            "forced_capabilities": (
                forced_capabilities if forced_capabilities is not None else []
            ),
            "exclusive_capability_match": exclusive_capability_match,
        },
    )


def seed_model_group(
    fake_web: "FakeWebClient",
    *,
    group: str,
    vendor_key_ids: list[str],
    strategy: str = "fixed_order",
    cross_vendor_fallback: bool = False,
    max_tokens_cap: Optional[int] = None,
    default_params: Optional[dict] = None,
    forced_params: Optional[dict] = None,
) -> None:
    """Write a single `model_groups` row.

    `vendor_key_ids` is a list because E4-C will register multiple keys
    under one group and switch `strategy` to `round_robin` / `weighted_rr` /
    `load_balance` / etc.
    """
    fake_web.put_entity(
        "model_groups",
        group,
        {
            "group": group,
            "strategy": strategy,
            "cross_vendor_fallback": cross_vendor_fallback,
            "max_tokens_cap": max_tokens_cap,
            "default_params": default_params if default_params is not None else {},
            "forced_params": forced_params if forced_params is not None else {},
            "vendor_key_ids": vendor_key_ids,
        },
    )


def seed_pricing(
    fake_web: "FakeWebClient",
    *,
    model: str,
    input_micro: int = 30000,
    output_micro: int = 60000,
    max_output: int = 4096,
    model_markup: float = 1.0,
    connect_timeout_ms: Optional[int] = None,
    read_timeout_ms: Optional[int] = None,
    total_timeout_ms: Optional[int] = None,
    stream_total_timeout_ms: Optional[int] = None,
) -> None:
    """Write a single `pricings` row. Defaults match a typical chat model
    (30k/60k micro-units per token, 4096 max output)."""
    fake_web.put_entity(
        "pricings",
        model,
        {
            "model": model,
            "input_price_micro_per_token": input_micro,
            "output_price_micro_per_token": output_micro,
            "max_output": max_output,
            "model_markup": model_markup,
            "connect_timeout_ms": connect_timeout_ms,
            "read_timeout_ms": read_timeout_ms,
            "total_timeout_ms": total_timeout_ms,
            "stream_total_timeout_ms": stream_total_timeout_ms,
        },
    )


# ---------------------------------------------------------------------------
# seed_passthrough — thin composition over the primitives above for the
# happy-path one-key same-protocol case. The signature and resulting
# payloads are IDENTICAL to the pre-refactor inline version, so the E3-B
# happy-path tests are unaffected. New scenario suites (E4-C / E4-D) that
# need multiple keys, alternate strategies, or cross-protocol endpoints
# should call the primitives directly instead of extending this helper.
# ---------------------------------------------------------------------------


def seed_passthrough(
    fake_web: "FakeWebClient",
    *,
    protocol: str,
    model: str,
    group: str,
    vendor_id: str,
    key_id: str,
    user_id: str,
    api_key_id: str,
    api_key_plaintext: str,
    upstream_key: str = "sk-up",
    base_url: str = "http://fake-upstream:9000",
) -> dict:
    """Seed users + api_keys + vendor + vendor_key + model_group + pricing
    so a pair-auth request for `model` over `protocol` routes to `base_url`.

    `protocol` must be the snake_case ProtocolKind variant the gateway
    expects: `open_ai`, `anthropic`, or `gemini` (NOT `openai` — see
    serde rename_all="snake_case" on `gw_core::protocol::ProtocolKind`).
    The string is passed straight through to `accessible_endpoints`.

    Returns a small dict of the ids that were seeded for the caller's
    convenience.
    """
    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    seed_vendor(fake_web, vendor_id=vendor_id, base_urls=[base_url])
    seed_vendor_key(
        fake_web,
        key_id=key_id,
        vendor_id=vendor_id,
        model=model,
        accessible_endpoints=[protocol],
        upstream_key=upstream_key,
        protocol_conversion=False,
    )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=[key_id],
        strategy="fixed_order",
    )
    seed_pricing(fake_web, model=model)

    return {
        "user_id": user_id,
        "api_key_id": api_key_id,
        "api_key_plaintext": api_key_plaintext,
        "vendor_id": vendor_id,
        "key_id": key_id,
        "group": group,
        "model": model,
    }
