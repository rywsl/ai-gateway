"""E4-E: reliability black-box scenarios (3).

Exercises three reliability invariants of the gateway end-to-end:

1. **DLQ on `usage:records` XADD failure** — when the primary stream key
   cannot accept an XADD (here forced by pre-creating it as a plain string
   so Redis returns `WRONGTYPE`), the ledger flusher wraps each record into
   `usage:records:dlq` with `dlq_meta` and increments
   `gw_usage_dlq_enqueued_total{outcome="success"}`. The wire request must
   still succeed — the failure is purely on the post-stream usage XADD.
   See `crates/gw-ledger/src/flusher.rs:99-176` (dlq_xadd + flush) and the
   reference unit test at `crates/gw-ledger/tests/dlq.rs`.

2. **`events:entity` cache invalidation propagates within ~1s** — the
   fake_web `put_entity` writes the row, deletes the gateway's Redis L1
   cache key `entity:{ns}:{id}`, and XADDs `{ns, id}` to `events:entity`.
   The gateway's `StreamConsumer` polls `events:entity` with `BLOCK_MS=200`
   (`crates/gw-store/src/stream_consumer.rs:33`) and on receipt
   *invalidates* the Moka cache entry. The next request cold-loads from
   PG. We assert: first request (api_key.allowed_models=[MODEL]) succeeds,
   we flip allowed_models to a model that doesn't include MODEL, poll up
   to ~1s for invalidation, then the next request 403s with
   `GW_REQUEST_MODEL_NOT_FOUND` (the code emitted at
   `crates/gw-proxy/src/filters/request.rs:574`).

3. **Gateway container restart recovers cold-load chain** — restarting
   ONLY the gateway container preserves PG + Redis (docker-compose has no
   `-v` and the volumes are container-internal). The boot-seeded
   `auth_configs.global` row stays in PG, the seeded entity graph stays in
   PG, and on restart the gateway rebuilds its `JwtValidator` from PG and
   cold-loads entities on demand. A second request after `/readyz` returns
   200 must round-trip successfully — proving the cold-load chain
   (`*_or_load` in `gw-store::entity_store`) works after a fresh process.
"""

from __future__ import annotations

import subprocess
import time

import pytest
import requests

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
    seed_passthrough,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
    sha256_bytes,
    wait_for_records,
)


# ---------------------------------------------------------------------------
# Compose-exec helpers. We never use `docker exec <container>` because the
# container name is project-name-dependent; resolving through
# `docker compose ... exec <service>` keeps these tests robust against
# `-p <project>` overrides. The compose file lives one level up from the
# pytest cwd (`tests/e2e/client/`).
# ---------------------------------------------------------------------------


_COMPOSE_FILE = "../docker-compose.yml"


def _redis_cli(*args: str, timeout: float = 10) -> subprocess.CompletedProcess[str]:
    """Run `redis-cli <args>` inside the compose `redis` service."""
    return subprocess.run(
        [
            "docker",
            "compose",
            "-f",
            _COMPOSE_FILE,
            "exec",
            "-T",
            "redis",
            "redis-cli",
            *args,
        ],
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def _restart_gateway() -> None:
    """`docker compose restart gateway` — sync, no `-v`, no rebuild."""
    proc = subprocess.run(
        [
            "docker",
            "compose",
            "-f",
            _COMPOSE_FILE,
            "restart",
            "gateway",
        ],
        capture_output=True,
        text=True,
        timeout=60,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"docker compose restart gateway failed (rc={proc.returncode}): "
            f"stderr={proc.stderr!r} stdout={proc.stdout!r}"
        )


def _wait_for_readyz(timeout: float = 30.0) -> float:
    """Poll `/readyz` until 200 or timeout. Returns elapsed seconds.

    The gateway re-init touches PG + Redis (rebuilds JwtValidator, recreates
    consumer groups, warms the metric registry). Empirically takes a few
    seconds; allow up to 30s — well above the auth-test budgets because
    full bootstrap is heavier than the per-request work those scenarios
    exercise.
    """
    deadline = time.monotonic() + timeout
    start = time.monotonic()
    while time.monotonic() < deadline:
        try:
            r = requests.get("http://localhost:8080/readyz", timeout=2)
            if r.status_code == 200:
                return time.monotonic() - start
        except requests.RequestException:
            pass
        time.sleep(0.25)
    raise TimeoutError(f"/readyz did not return 200 within {timeout}s")


# ---------------------------------------------------------------------------
# 1. DLQ grows when usage:records is corrupt (non-stream key).
#
# The pre-existing reference test (`crates/gw-ledger/tests/dlq.rs`) sets
# `usage:records` to a plain string and asserts XADDs cascade to the DLQ.
# We mirror that exact pattern at the integration boundary — via
# `redis-cli SET usage:records blocked` inside the compose redis container —
# and drive ONE streaming request through the gateway. The wire response
# succeeds (upstream call is healthy); the failure is the post-stream
# ledger XADD, which the flusher catches and routes to `usage:records:dlq`,
# stamping `gw_usage_dlq_enqueued_total{outcome="success"}` on the way.
# ---------------------------------------------------------------------------


def test_dlq_grows_when_usage_records_corrupt(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    seed_passthrough(
        fake_web,
        protocol=PROTO_OPENAI,
        model=MODEL,
        group="rel-dlq-grp",
        vendor_id="v-rel-dlq",
        key_id="vk-rel-dlq",
        user_id="u-rel-dlq",
        api_key_id="ak-rel-dlq",
        api_key_plaintext="sk-rel-dlq",
    )
    fake_web.set_balance("u-rel-dlq", BALANCE_LARGE)
    # fake_upstream serves the default happy SSE (isolate_state restored it).

    # Corrupt the primary stream BEFORE the request so the flusher's XADD
    # hits WRONGTYPE on the FIRST batch this test produces. `isolate_state`
    # FLUSHALL'd Redis at the top of the test, so there's no
    # `usage:records` stream key to clash with — SET creates a fresh string
    # key. The explicit DEL in `finally` is paranoia for the next test:
    # `isolate_state` runs `reset()` BEFORE every test, so the next test
    # cleans up too — but if `isolate_state` ever changed to "selective
    # cleanup that preserves event streams" (as the store.rs comment hints
    # it might for events:entity), an explicit out-of-band string key
    # would survive. Belt + braces.
    try:
        proc = _redis_cli("SET", "usage:records", "blocked")
        assert proc.returncode == 0 and proc.stdout.strip() == "OK", (
            f"SET usage:records failed: rc={proc.returncode} "
            f"stdout={proc.stdout!r} stderr={proc.stderr!r}"
        )

        pre = metrics_snapshot("gw_usage_dlq_enqueued_total", metrics_url)

        # Drive one streaming request — must succeed at the wire level.
        client = make_openai_client(gateway_url, "sk-rel-dlq", "ak-rel-dlq")
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

        # Poll the DLQ XLEN via redis-cli directly. We can't use the
        # fake_web `/usage/stats` surface here because it internally does
        # `XLEN usage:records` first, and XLEN on a string key returns
        # WRONGTYPE — the handler 500s on the records leg before reaching
        # the dlq leg. `docker exec redis-cli XLEN usage:records:dlq`
        # talks directly to the stream and is unaffected by the corrupted
        # `usage:records` string key. The ledger flush is batched (~500ms
        # interval in v1) so 5s is comfortable headroom.
        deadline = time.monotonic() + 5.0
        dlq_xlen = 0
        while time.monotonic() < deadline:
            proc = _redis_cli("XLEN", "usage:records:dlq")
            if proc.returncode != 0:
                # Mirror the _redis_xrange pattern in test_routing_retry.py:
                # a non-zero rc means redis-cli itself misfired (compose
                # service down, container exec failure, …) — surface it
                # instead of silently spinning and tripping the final
                # assert with dlq_xlen=0.
                raise RuntimeError(
                    f"redis-cli XLEN usage:records:dlq failed "
                    f"(rc={proc.returncode}): stdout={proc.stdout!r} "
                    f"stderr={proc.stderr!r}"
                )
            try:
                dlq_xlen = int(proc.stdout.strip())
            except ValueError:
                dlq_xlen = 0
            if dlq_xlen >= 1:
                break
            time.sleep(0.1)
        assert dlq_xlen >= 1, (
            f"expected usage:records:dlq XLEN >=1 after corrupt-primary "
            f"streaming request; got {dlq_xlen}."
        )

        # The metric MUST have incremented on the `success` outcome (XADD
        # to DLQ succeeded). The flusher's failure-mode metric is
        # `outcome="error"` (DLQ XADD itself failed) — neither error path
        # should fire here because Redis IS alive.
        post = metrics_snapshot("gw_usage_dlq_enqueued_total", metrics_url)
        delta_success = label_delta(pre, post, 'outcome="success"')
        assert delta_success >= 1, (
            f"expected gw_usage_dlq_enqueued_total{{outcome=\"success\"}} to "
            f"increment by >=1 for the corrupt-primary streaming request; "
            f"pre={pre}, post={post}"
        )
    finally:
        # MUST delete the string key so subsequent tests can XADD to
        # usage:records normally. `isolate_state.reset()` FLUSHALL's at
        # the START of the next test, but that's later; better to leave a
        # clean Redis than rely on the next reset.
        _redis_cli("DEL", "usage:records")


# ---------------------------------------------------------------------------
# 2. `events:entity` cache invalidation propagates within ~1s.
#
# Spec: on a successful `put_entity`, the fake_web (a) deletes the gateway's
# Redis L1 cache for `entity:apikey:{id}` and (b) XADDs `{ns,id}` to
# `events:entity`. The gateway's StreamConsumer polls with BLOCK_MS=200ms
# and invalidates the corresponding Moka entry. The next request cold-loads
# from PG and sees the new payload.
#
# Test shape: request 1 must succeed (proves the entity reached the Moka
# cache); flip api_key.allowed_models to a model that the request model is
# NOT in; poll up to ~1s; request 2 must 403 with GW_REQUEST_MODEL_NOT_FOUND.
# Note: api_keys.allowed_models is NOT the same as users.allowed_models —
# `seed_user_apikey` distinguishes them via `api_key_allowed_models` vs
# `allowed_models`. Auth merge intersects allowed_models per tier where
# `[]` is unrestricted; flipping the api_key tier to `["NEVER-MATCH-MODEL"]`
# makes the merged set NOT contain MODEL even though the user tier is
# unrestricted. The merged-permission code path is at
# `crates/gw-auth/src/merge.rs:58-66`.
# ---------------------------------------------------------------------------


def test_entity_update_invalidates_cache_within_1s(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    # Seed with api_key.allowed_models=[MODEL] — explicit allowlist so we
    # can flip it. The user-tier allowed_models stays empty (unrestricted)
    # so the merged intersection is dominated by the api_key tier.
    seed_user_apikey(
        fake_web,
        user_id="u-rel-inv",
        api_key_id="ak-rel-inv",
        api_key_plaintext="sk-rel-inv",
        allowed_groups=["rel-inv-grp"],
        api_key_allowed_models=[MODEL],
    )
    fake_web.set_balance("u-rel-inv", BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id="v-rel-inv")
    seed_vendor_key(
        fake_web,
        key_id="vk-rel-inv",
        vendor_id="v-rel-inv",
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_model_group(
        fake_web,
        group="rel-inv-grp",
        vendor_key_ids=["vk-rel-inv"],
        strategy="fixed_order",
    )
    seed_pricing(fake_web, model=MODEL)

    # Request 1 — succeeds AND populates the gateway's Moka cache for
    # `entity:apikey:ak-rel-inv` (and the api_key row goes through the
    # merge path so the resolved `allowed_models` slice is observed).
    client = make_openai_client(gateway_url, "sk-rel-inv", "ak-rel-inv")
    stream1 = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    pieces1: list[str] = []
    for chunk in stream1:
        if chunk.choices and chunk.choices[0].delta.content:
            pieces1.append(chunk.choices[0].delta.content)
    assert "".join(pieces1) == "hi", pieces1

    # Flip api_key.allowed_models to a model that is NOT MODEL. ALL other
    # fields must remain in the seed_user_apikey defaults (status active,
    # rpm_limit 0, hash unchanged, etc.) — we copy the full payload shape
    # `seed_user_apikey` writes for `api_keys`. Hash must match the same
    # plaintext or auth will fail with INVALID_APIKEY before model-permission
    # ever runs.
    fake_web.put_api_key(
        "ak-rel-inv",
        key_id="ak-rel-inv",
        user_id="u-rel-inv",
        status="active",
        hash=sha256_bytes("sk-rel-inv"),
        allowed_models=["NEVER-MATCH-MODEL"],
        allowed_channels=[],
        rpm_limit=0,
        tpm_limit=0,
        markup=1.0,
        ip_allowlist=None,
        expires_at=None,
        signature_required=False,
        signature_algo="hmac_sha256",
        force_capabilities=[],
        match_mode_exclusive=False,
    )

    # Poll up to ~1s for the gateway consumer to invalidate. We can't
    # observe the Moka cache directly; instead we send request 2 in a
    # tight loop and accept as "invalidated" the first response that
    # 403s with GW_REQUEST_MODEL_NOT_FOUND. The stream-consumer poll
    # period is BLOCK_MS=200ms, so 1s = 5 consumer ticks of headroom.
    deadline = time.monotonic() + 1.0
    last_resp = None
    while time.monotonic() < deadline:
        resp = post_chat(
            gateway_url,
            pair_headers("sk-rel-inv", "ak-rel-inv"),
            {
                "model": MODEL,
                "messages": [{"role": "user", "content": "hi"}],
                "stream": True,
            },
            timeout=10,
        )
        last_resp = resp
        if resp.status_code == 403 and _error_code(resp) == "GW_REQUEST_MODEL_NOT_FOUND":
            break
        # 50ms inner cadence × 20 ticks = 1s deadline (BLOCK_MS=200,
        # 5 consumer ticks of headroom).
        time.sleep(0.05)

    assert last_resp is not None
    assert last_resp.status_code == 403, (
        f"expected 403 after api_key allowed_models flipped to "
        f"['NEVER-MATCH-MODEL']; got status={last_resp.status_code} "
        f"body={last_resp.text[:300]!r}. If you saw 200 the whole second, "
        f"the gateway's events:entity cache invalidation did not propagate "
        f"within 1s — spec violation worth investigating in "
        f"crates/gw-store/src/stream_consumer.rs."
    )
    assert _error_code(last_resp) == "GW_REQUEST_MODEL_NOT_FOUND", last_resp.text[:300]


# ---------------------------------------------------------------------------
# 3. Gateway container restart recovers the cold-load chain.
#
# `docker compose restart gateway` kills and re-starts ONLY the gateway
# container. Postgres + Redis volumes are container-internal and not
# `-v`-removed, so all seeded entity rows + balance counters + the
# `auth_configs.global` row survive. The gateway re-bootstraps:
#   - rebuilds JwtValidator from PG (auth_configs);
#   - re-creates `events:entity`/`events:counter` consumer groups (BUSYGROUP
#     tolerated);
#   - cold-loads each entity row via `*_or_load` on first request.
#
# Asserts: pre-restart request succeeds → restart → /readyz → post-restart
# request succeeds (same payload).
#
# Unique ids guard against any cross-test cache leakage in the OLD gateway
# process (Moka cache dies with the container, but counters and entity rows
# would have collided with rel-dlq or rel-inv if we reused them).
# ---------------------------------------------------------------------------


def test_gateway_container_restart_recovers(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    # NOTE: no auth_config re-seed needed here. The boot-time SQL in
    # `tests/e2e/seed_auth_config.sql` runs once on a FRESH postgres data
    # dir (initdb-once semantics), and the per-test `isolate_state.reset()`
    # deliberately PRESERVES `auth_configs` — it TRUNCATEs `TRUNCATE_TABLES`,
    # which is `ENTITY_TABLES` minus `auth_configs` (see
    # `crates/e2e-fake-web/src/store.rs`). So the boot-seeded global
    # `auth_configs.global` row survives every reset and is still in PG
    # when the restarted gateway runs `auth_config_or_load("global")` on
    # its cold path. The JwtValidator wiring therefore survives the
    # restart without any harness-side re-seed.
    seed_passthrough(
        fake_web,
        protocol=PROTO_OPENAI,
        model=MODEL,
        group="rel-rst-grp",
        vendor_id="v-rel-rst",
        key_id="vk-rel-rst",
        user_id="u-rel-rst",
        api_key_id="ak-rel-rst",
        api_key_plaintext="sk-rel-rst",
    )
    fake_web.set_balance("u-rel-rst", BALANCE_LARGE)

    # Pre-restart request — proves the seed graph is wired and the
    # current gateway process can serve.
    client_pre = make_openai_client(gateway_url, "sk-rel-rst", "ak-rel-rst")
    stream_pre = client_pre.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    pieces_pre: list[str] = []
    for chunk in stream_pre:
        if chunk.choices and chunk.choices[0].delta.content:
            pieces_pre.append(chunk.choices[0].delta.content)
    assert "".join(pieces_pre) == "hi", pieces_pre
    # Confirm at least one usage:record landed (sanity check before the
    # process replacement).
    assert wait_for_records(fake_web, min_xlen=1, timeout=3.0) >= 1

    # Restart only the gateway. PG + Redis stay up.
    _restart_gateway()
    readyz_elapsed = _wait_for_readyz(timeout=30.0)

    # Post-restart request — must succeed via cold-load chain. Use a new
    # OpenAI client because the previous client's httpx pool may hold a
    # dead connection to the old gateway process; a fresh client ensures
    # a clean TCP connect to the new process.
    client_post = make_openai_client(gateway_url, "sk-rel-rst", "ak-rel-rst")
    stream_post = client_post.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    pieces_post: list[str] = []
    for chunk in stream_post:
        if chunk.choices and chunk.choices[0].delta.content:
            pieces_post.append(chunk.choices[0].delta.content)
    assert "".join(pieces_post) == "hi", (pieces_post, readyz_elapsed)
