"""T4-C (G6) docker e2e: `events:entity` invalidation DELs the Redis L1
entry, not just the in-process Moka L1.

Validates the G6 deliverable end-to-end against a live gateway brought up
by `make e2e-up`:

- Each entity namespace caches `payload` in Redis L1 at
  `entity:{ns}:{id}` (HSET; see `crates/gw-store/src/loader.rs::load_entity`,
  line 43). Multi-pod deployments share this layer.
- Before G6 (T1-C), `apply_entity_event` only invalidated the per-process
  Moka L0 cache. A second pod could keep serving the stale `entity:*`
  hash until the TTL elapsed (docs/architecture.md §10.5).
- After G6: `apply_entity_event` (`crates/gw-store/src/stream_consumer.rs`,
  line 174) `DEL`s the Redis key BEFORE the Moka invalidate. Redis DEL
  failure is downgraded to a `warn!` so Moka still runs.

----------------------------------------------------------------------------
Sequence
----------------------------------------------------------------------------
  1. `put_entity("api_keys", "ak_test_l1", { ...active... })`. fake-web
     UPSERTs PG, then XADDs `events:entity {ns=apikey, id=ak_test_l1}`.
     The gateway's stream_consumer may DEL a non-existent Redis key,
     which is a no-op (DEL on missing returns 0). This step is necessary
     so the row exists in PG before the cold-load tries to read it.
  2. Drive a single request through the gateway with the seeded api-key
     id. The request will hit the auth pipeline, which calls
     `EntityStore::apikey_or_load(ak_test_l1)`. The loader misses Moka
     (just invalidated), misses Redis (just DEL'd), reads PG, writes
     the Redis L1 (`HSET entity:apikey:ak_test_l1 ...`) and the Moka L0,
     and returns the row. The downstream auth check will FAIL (we don't
     know the real plaintext hash) — that's fine. What matters is that
     the cold-load ran.
  3. Assert `EXISTS entity:apikey:ak_test_l1 == 1`.
  4. `put_entity("api_keys", "ak_test_l1", { ...disabled... })`. The
     status change is incidental; what fake-web cares about is that the
     UPSERT fires `events:entity {ns=apikey, id=ak_test_l1}`.
  5. Wait for the gateway's consumer to process the event (XREADGROUP
     block + DEL is bounded by ~200 ms; we wait 1.5 s for slack).
  6. Assert `EXISTS entity:apikey:ak_test_l1 == 0`. G6 invariant.

Step 2's "any request" — we use a bare HTTP POST with deliberately wrong
credentials. The 401/403 result is irrelevant; the assertion is on the
Redis side-effect of `apikey_or_load`. We DO NOT need a full user / vendor
/ vendor_key seed for this test.

----------------------------------------------------------------------------
Wire conventions reused
----------------------------------------------------------------------------
* `ns_for_table("api_keys") = "apikey"` (see
  `crates/e2e-fake-web/src/store.rs::ns_for_table`). The Redis key is
  therefore `entity:apikey:<id>`, NOT `entity:api_keys:<id>` and NOT
  `cache:apikey:<id>` (the old `cache:*` prefix was renamed in T3-pre,
  commit `fca1d4a`).
* `signature_algo` defaults to `hmac_sha256`; we set it explicitly so
  the payload survives any future serde-default tightening.
* The `expires_at` field's wire format on `api_keys` is the
  `time::OffsetDateTime` default-serde tuple (NOT RFC3339; see the
  `_expires_at_tuple` helper in conftest used by mgmt tokens). Passing
  `None` here keeps the row simple and avoids the tuple plumbing.
"""
from __future__ import annotations

import time

import requests

from conftest import FakeWebClient, sha256_bytes


# Redis HSET fields that `load_entity` always writes (see loader.rs,
# line 75-78). At minimum `d` (payload) and `v` (version) are present.
_APIKEY_TABLE = "api_keys"
_APIKEY_NS = "apikey"  # ns_for_table("api_keys") == "apikey"
_APIKEY_ID = "ak_test_l1"
_REDIS_KEY = f"entity:{_APIKEY_NS}:{_APIKEY_ID}"


def _apikey_payload(*, status: str) -> dict:
    """Minimal-but-valid `ApiKeyEntry` payload.

    `force_capabilities` (not `forced_capabilities`) — matches the
    `gw-core::entity::ApiKeyEntry` field name (see entity.rs, line 119).
    """
    return {
        "key_id": _APIKEY_ID,
        "user_id": "u_test_l1",
        "status": status,
        # `hash` is `Vec<u8>` — serde-json from a JSON int array.
        "hash": sha256_bytes("sk-test-l1-unused"),
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
    }


def test_apikey_invalidate_deletes_redis_l1(
    gateway_url: str,
    fake_web: FakeWebClient,
    redis_client,
) -> None:
    """G6 (T1-C): an `events:entity` invalidation DELs the Redis L1
    key, not just the per-process Moka cache."""
    # 1. Seed an apikey row.
    fake_web.put_entity(_APIKEY_TABLE, _APIKEY_ID, _apikey_payload(status="active"))

    # 2. Trigger a cold-load by issuing any request that hits the auth
    #    pipeline with this api-key-id. The auth check will fail (wrong
    #    plaintext) — we only need the loader to run as a side-effect.
    #    The pair-auth path runs `apikey_or_load(ak_test_l1)` regardless
    #    of subsequent rejection.
    requests.post(
        f"{gateway_url.rstrip('/')}/v1/chat/completions",
        headers={
            "x-api-key": "any-plaintext-irrelevant",
            "x-api-key-id": _APIKEY_ID,
            "content-type": "application/json",
        },
        json={
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "x"}],
            "stream": False,
        },
        timeout=10,
    )
    # The loader cold-load completes synchronously inside the request
    # filter (it blocks the filter chain). The Redis HSET is awaited
    # before the loader returns — see loader.rs::load_entity. By the
    # time the gateway responds (whatever the status), the key exists.
    # We still poll briefly in case the rejection raced ahead in a way
    # that bypasses the loader (e.g. fast-fail rate-limit shielding).
    deadline = time.monotonic() + 2.0
    while time.monotonic() < deadline:
        if redis_client.exists(_REDIS_KEY) == 1:
            break
        time.sleep(0.05)
    assert redis_client.exists(_REDIS_KEY) == 1, (
        f"expected Redis L1 key `{_REDIS_KEY}` after cold-load, "
        f"got EXISTS={redis_client.exists(_REDIS_KEY)}. "
        "Cold-load may have been bypassed or the prefix has changed."
    )

    # 3. Re-put the apikey row. fake-web upserts PG and XADDs
    #    `events:entity {ns=apikey, id=ak_test_l1}`. Status change to
    #    `disabled` is incidental — the XADD fires on ANY upsert.
    fake_web.put_entity(
        _APIKEY_TABLE,
        _APIKEY_ID,
        _apikey_payload(status="disabled"),
    )

    # 4. Wait for the gateway's `StreamConsumer` to read the event and
    #    DEL the Redis L1 entry. The consumer's XREADGROUP blocks up to
    #    ~200 ms; 1.5 s gives generous slack for scheduling jitter +
    #    multiple consumers / partitions.
    deadline = time.monotonic() + 1.5
    while time.monotonic() < deadline:
        if redis_client.exists(_REDIS_KEY) == 0:
            break
        time.sleep(0.05)

    assert redis_client.exists(_REDIS_KEY) == 0, (
        f"expected Redis L1 key `{_REDIS_KEY}` to be DEL'd within 1.5s "
        f"after events:entity invalidate, got EXISTS="
        f"{redis_client.exists(_REDIS_KEY)}. "
        "Regression in apply_entity_event (T1-C / G6)?"
    )
