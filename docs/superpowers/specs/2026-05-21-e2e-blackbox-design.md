# 2026-05-21 — End-to-End Black-Box Test Harness Design

> **Status**: spec only. Implementation plan lives at `docs/superpowers/plans/2026-05-21-e2e-blackbox.md`.
>
> **Scope**: build a docker-based E2E harness so the gateway can be exercised through real network sockets, real Redis/Postgres, real Go-side control-plane events, and real client SDKs. Not in scope: CI workflow files (project is pre-stable; the harness is run by humans + a `make` target).
>
> **Why this exists**: today's E2E surface is `crates/gw-gateway/tests/e2e_full_flow.rs` (`#[ignore]`, in-process `wiremock` + `reqwest`). It cannot exercise: Pingora's `fail_to_connect` retry, real TLS, SSE wire-level chunking, concurrent multi-client behaviour, cross-pod cache invalidation via `events:entity`, balance write-back from Go, JWKS rotation, container restart recovery. Each `known-gaps` PR will add fixes whose correctness is only observable end-to-end.

## 0. Three-actor model

```
                                  Docker network: aether-e2e
   ┌──────────────────────┐     ┌──────────────────────┐
   │  fake_client (host)  │     │  fake_web_server     │
   │  Python + 3 SDKs     │     │  Rust binary         │
   │  (openai / anthropic │     │  Simulates Go web    │
   │   / google-genai)    │     │  boundary            │
   └──────────┬───────────┘     └──────────┬───────────┘
              │ HTTP                       │ Postgres write
              │ (port 8000)                │ Redis XADD/XREAD
              ▼                            ▼
        ┌──────────────────────────────────────┐
        │  gw-gateway (under test)             │
        │  Production binary, no test hooks    │
        │  Ports: 8000/9090/8080               │
        └────────────────┬──────────────┬──────┘
                         │              │
                    TCP/TLS         Redis/PG
                         │              │
                         ▼              ▼
        ┌──────────────────────┐   ┌──────────────┐
        │  fake_upstream_server│   │ redis + pg   │
        │  Rust binary         │   │ (official    │
        │  Simulates OpenAI /  │   │  containers) │
        │  Anthropic / Gemini  │   └──────────────┘
        └──────────────────────┘
```

Three new components — all **fakes** sitting at the gateway's external boundaries:

1. **`fake_upstream_server`** (Rust, `crates/e2e-fake-upstream/`) — pretends to be the LLM vendor. Returns configurable JSON/SSE responses, can inject failures (RST, half-write, slow, 4xx/5xx, `Retry-After`, malformed SSE).
2. **`fake_web_server`** (Rust, `crates/e2e-fake-web/`) — pretends to be the Go control plane. Writes entities to Postgres, fires `events:entity` for cache invalidation, consumes `usage:records` to compute mock cost and write back `counter:user:*:balance_micro` (the path that triggers `USER_CIRCUIT` and `GW_QUOTA_BALANCE_ZERO`). Exposes a small admin HTTP API so scenarios can drive it.
3. **`fake_client`** (Python, `tests/e2e/client/`) — uses the three real vendor SDKs (`openai`, `anthropic`, `google-generativeai`) to talk to the gateway at `http://localhost:8000`. Pytest is the runner.

The gateway runs **unmodified production binary** — there is no test hook, conditional compilation, or magic header. If we can exercise behaviour from this harness, it is real. If we cannot, no v1 deployment can either.

## 1. Why each fake exists

### 1.1 `fake_upstream_server`

A `wiremock` Rust crate runs in-process and can't:

- emit partial chunks at arbitrary byte offsets (Pingora's SSE parser is byte-stream; v1 has a leftover buffer that must be exercised);
- reset the TCP connection mid-response;
- close the connection during TLS handshake;
- send a `Content-Length`-truncated body;
- respond with a delay greater than its single-threaded poll budget without starving the gateway runtime;
- listen on a real socket so Pingora's `fail_to_connect` actually fires;
- ship inside a container alongside the gateway.

`fake_upstream_server` is a stand-alone Rust binary built on `hyper` (no Pingora — it's the *upstream*, not a proxy) with a small **scenario DSL** loaded at startup from JSON / TOML.

### 1.2 `fake_web_server`

Production gateway depends on the Go web service at four boundaries:

| Boundary | Direction | What gateway sees |
|----------|-----------|-------------------|
| Postgres entity rows | Go writes, gateway reads | `EntityStore::*_or_load` cold path |
| `events:entity` Redis stream | Go XADD, gateway XREADGROUP | `StreamConsumer::apply` cache invalidation |
| `usage:records` Redis stream | Gateway XADD, Go XREAD | Billing — but **no** observation if Go is absent |
| `counter:user:{id}:balance_micro` Redis key | Go SET, gateway GET | Balance enforcement (`GW_QUOTA_BALANCE_ZERO`) |
| JWKS endpoint or DB row | Go writes, gateway reads via `EntityStore::jwks_or_load` | JWT validation |

Without `fake_web_server`, we cannot test:

- balance going to 0 mid-traffic → `USER_CIRCUIT.Open` short-circuit;
- entity update → cache eviction → next request sees new permissions;
- JWKS rotation → old `kid` rejected, new `kid` accepted;
- daily-quota counter rolling at UTC midnight;
- DLQ recovery (gateway writes `usage:records:dlq`; Go-equivalent must drain or scenarios verify XLEN).

`fake_web_server` is a Rust binary with:

- A Postgres connection (writes via raw SQL).
- A Redis connection (XADD / SET / DEL).
- An admin HTTP API on port `7000`:
  - `POST /entity/{table}` — write a row + emit `events:entity`.
  - `DELETE /entity/{table}/{id}` — soft-delete row + emit invalidation.
  - `POST /counter/{scope}/{id}/{field}` — set the counter to a value.
  - `POST /balance/{user_id}` — set `balance_micro` (shortcut for the common case).
  - `POST /usage/drain` — drain `usage:records` and mock-compute cost into `counter:user:*:balance_micro` (auto-runs as a background loop by default; this endpoint forces an immediate cycle).
  - `GET /usage/stats` — current XLEN of `usage:records`, `usage:records:dlq`, `usage:detailed`.
  - `POST /jwks/rotate` — replace JWKS entity with a new keypair, return the private key for the client to sign with.

It is **not** a faithful Go replica. It is the minimum surface the gateway leans on, scripted to drive scenarios.

### 1.3 `fake_client`

Three reasons it's Python with real SDKs:

1. **Black-box wire compliance**: the three vendor SDKs are the real-world consumers. If the gateway's response shape is technically valid JSON but the SDK can't parse it, that's a bug. Reuse of `gw-codec` types on the client side would mask such bugs.
2. **Async streaming**: each SDK has its own iterator / async-iterator semantics for SSE. Driving those directly proves the wire is correctly framed.
3. **Test ergonomics**: pytest + parametrised tests is faster to write scenarios in than `cargo test`.

Tradeoff: maintaining `requirements.txt` for three SDKs. Acceptable; pinned versions in the plan.

## 2. ADRs

### ADR-E1-1 — Docker compose topology

Single `docker-compose.yml` at `tests/e2e/docker-compose.yml`:

```
services:
  postgres:    # official postgres:16-alpine, healthcheck on pg_isready
  redis:       # official redis:7-alpine, healthcheck on redis-cli ping
  fake-upstream:   # built from crates/e2e-fake-upstream
  fake-web:        # built from crates/e2e-fake-web, depends_on: [postgres healthy, redis healthy]
  gateway:         # built from project Dockerfile, depends_on: [redis healthy, postgres healthy, fake-web healthy]
```

The gateway image **is** the production `target/release/gw-gateway` binary in a `gcr.io/distroless/cc` (or `debian-slim`) base. No test-only flags. Project Dockerfile lives at `Dockerfile` (root) — added in this work if not already present.

`fake_client` is **not** in compose. It runs from the host (`pytest tests/e2e/`) and talks to `localhost:8000`. Reasoning: keeps Python deps off the container set, and allows IDE debugging of test code without rebuilding an image. The host-network model also lets pytest see real wall-clock latencies.

A single `make e2e` target:

```makefile
e2e:
\tdocker compose -f tests/e2e/docker-compose.yml up -d --build
\t./tests/e2e/wait_for_ready.sh
\tcd tests/e2e && python -m pytest -v
\tdocker compose -f tests/e2e/docker-compose.yml down -v
```

No CI workflow file. The harness runs on developer machines.

### ADR-E1-2 — Scenario configuration shape

`fake_upstream_server` and `fake_web_server` both load a **scenario document** at startup. JSON over the admin API so pytest can rewrite mid-test.

`fake_upstream_server` scenario DSL (illustrative):

```json
{
  "routes": [
    {
      "match": { "method": "POST", "path_regex": "^/v1/chat/completions$" },
      "auth_check": { "header": "Authorization", "must_start_with": "Bearer sk-up-" },
      "response": {
        "delay_ms": 0,
        "status": 200,
        "headers": { "content-type": "text/event-stream" },
        "sse": [
          { "data": "{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}", "after_ms": 10 },
          { "data": "[DONE]", "after_ms": 5 }
        ]
      }
    },
    {
      "match": { "header_eq": ["x-test-scenario", "rst-after-headers"] },
      "response": { "kind": "tcp_reset_after_headers" }
    }
  ]
}
```

Failure injection primitives:

- `tcp_reset_after_headers`
- `tcp_reset_mid_body`
- `connection_refused_first_n` (the listener accepts but RSTs the first N TCP connections — exercises `fail_to_connect`)
- `slow_body_byte_per_second` (drip-feed to exercise timeouts)
- `partial_sse_event` (write only half of `\n\n` boundary then close)
- `status_with_retry_after` (return 429 + `Retry-After: 5`)

`fake_web_server` accepts admin-API mutations rather than a static scenario; pytest writes entities, mutates counters, rotates JWKS in real time.

### ADR-E1-3 — Crate layering and workspace impact

Both fake servers live in **new workspace member crates**:

- `crates/e2e-fake-upstream/` (`bin = "fake_upstream"`)
- `crates/e2e-fake-web/` (`bin = "fake_web"`)

They may depend on:

- `gw-core` (for `ids::*` only — no behaviour reuse).
- `bytes`, `hyper`, `tokio`, `serde`, `serde_json`, `fred`, `sqlx`.

They may **not** depend on:

- `gw-codec` (the test must be wire-faithful; reusing the gateway's codec masks bugs);
- `gw-proxy`, `gw-gateway` (avoid bidirectional dep);
- `gw-auth`, `gw-store`, `gw-ledger` (same reason).

The dependency direction is clean: fakes are Execution-layer leaves that don't feed back into the build.

`scripts/check-invariants.sh` gains one lint: `e2e_fake_no_gw_codec_dep` — fakes must not import `gw-codec`.

### ADR-E1-4 — fake_client structure

`tests/e2e/`:

```
docker-compose.yml
wait_for_ready.sh             # poll /readyz until 200 or timeout
client/
  requirements.txt            # openai==1.x, anthropic==0.x, google-generativeai==0.x, pytest, pytest-asyncio
  conftest.py                 # fixtures: gateway_url, fake_web_admin, scenario builders
  scenarios/
    test_happy_streaming.py   # OpenAI/Anthropic/Gemini stream happy paths
    test_auth_failures.py     # expired_key, ip_allowlist, hmac, jwt_iss
    test_routing_retry.py     # connect_refused first key → second succeeds
    test_cross_protocol.py    # openai client + anthropic upstream, etc.
    test_quotas.py            # balance_zero, daily_quota, rpm, hard_caps
    test_jwks_rotation.py     # JWKS rotation mid-traffic
    test_cache_invalidation.py # update entity → next request sees new
    test_dlq.py               # corrupt usage:records, verify DLQ XADD
```

Each test file groups by behaviour, not by gap. A single gap (e.g. G10 retry) is covered by tests in `test_routing_retry.py` *and* potentially `test_cross_protocol.py` (retry under cross-shape).

Scenario builder fixtures (in `conftest.py`) expose a fluent shape:

```python
def test_first_key_connect_refused_falls_through_to_second(
    gateway_url, fake_upstream, fake_web
):
    # Two vendor keys behind one model_group. First key's upstream
    # rejects the first TCP connection.
    fake_web.put_vendor_key("k1", base_url=fake_upstream.url, connect_refused_first_n=1)
    fake_web.put_vendor_key("k2", base_url=fake_upstream.url)
    fake_web.put_model_group("free-tier", keys=["k1", "k2"], strategy="fixed_order")
    fake_web.put_api_key("ak1", user_id="u1", model_group="free-tier")
    fake_web.put_balance("u1", 1_000_000_000)

    client = openai.OpenAI(api_key="ak1", base_url=f"{gateway_url}/v1")
    resp = client.chat.completions.create(
        model="gpt-4", messages=[{"role": "user", "content": "hi"}], stream=True,
        extra_headers={"x-api-key-id": "ak1"},
    )
    chunks = list(resp)
    assert len(chunks) > 0
    metrics = fake_web.get_metrics()
    assert metrics["gw_retry_attempt_total"]["outcome=recovered"] == 1
```

The fixture abstracts admin-API calls; the test stays focused on observable behaviour.

### ADR-E1-5 — Scenario coverage (≥ 30 scenarios)

Minimum coverage at v1.5 PR-merge:

**Happy paths (6)** — 3 protocols × 2 (stream / passthrough). Non-stream stays `GW_REQUEST_NONSTREAM_NOT_SUPPORTED`; assertion confirms that.

**Auth (8)**:
- expired API key → 401 `GW_AUTH_KEY_EXPIRED`
- key + non-allow-listed IP → 403 `GW_AUTH_KEY_IP_NOT_ALLOWED`
- HMAC required, signature missing → 401 `GW_AUTH_SIGNATURE_MISSING`
- HMAC required, bad signature → 401 `GW_AUTH_SIGNATURE_INVALID`
- HMAC required, timestamp drifted > 300 s → 401 `GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT`
- JWT iss mismatch → 401 `GW_AUTH_JWT_INVALID_ISS`
- JWT aud mismatch → 401 `GW_AUTH_JWT_INVALID_AUD`
- JWKS rotated mid-stream — old `kid` rejected, new accepted

**Quota (6)**:
- balance zero before request → 402 `GW_QUOTA_BALANCE_ZERO`
- balance drained mid-stream by `fake_web` (USER_CIRCUIT trips on next request, not mid-stream)
- daily quota exhausted → 403 `GW_QUOTA_DAILY_TOKENS_EXCEEDED`
- daily quota rolls at UTC midnight (force a clock skew via `fake_web` admin)
- hard cap on input tokens → 400
- RPM exceeded → 429 `GW_QUOTA_RPM_EXCEEDED`

**Routing (5)**:
- connect refused → second key succeeds (retry recovered)
- both keys refused → 503 `GW_UPSTREAM_CONNECT_FAILED`
- TLS handshake error → 503 `GW_UPSTREAM_TLS_ERROR` (no retry)
- no key advertises required capability → 503 `GW_ROUTING_NO_CAPABILITY_MATCH`
- sticky binds same user to same key across two requests, expires after TTL

**Codec (5)**:
- cross-protocol stream: OpenAI client → Anthropic upstream
- cross-protocol stream: OpenAI client → Gemini upstream (Gemini combined-final-event decomposed correctly — regression guard for G9 + G8 collision)
- mid-stream upstream RST after some bytes → client sees truncated stream + retry not triggered
- malformed SSE (partial event) → gateway recovers / emits `GW_UPSTREAM_PROTOCOL_ERROR`
- gzip-encoded response decoded transparently (compression fast path)

**Reliability (3)**:
- `fake_web` artificially corrupts `usage:records` stream → DLQ XLEN grows; `gw_usage_dlq_enqueued_total` increments
- entity update via `fake_web.put_api_key(... permissions changed)` → next request sees new permissions within 1 s (cache invalidation via `events:entity`)
- container restart of gateway → next request succeeds (cold-load chain works)

Total: **33 scenarios** at v1.5 floor.

### ADR-E1-6 — Out of scope (for this spec)

- CI workflow (`.github/workflows/*`). Project pre-stable; humans drive `make e2e` locally.
- Load/perf testing. Use criterion benches separately.
- Chaos testing (random pod kills, network partitions). Phase 6 future spec.
- Multi-replica gateway. v1 is single-pod; multi-replica scenarios land alongside the Phase 6 deploy work.
- Fully-faithful Go cost computation. `fake_web` does a flat 1 µ/token; real markup chain is Go's problem.

### ADR-E1-7 — Migration / data shape

Both fakes use the **same Postgres schema** as the gateway (`crates/gw-store/migrations/`). Reasoning: the gateway reads the schema; the fake must write it. There is no separate test schema.

A small helper script `tests/e2e/run_migrations.sh` runs `psql -f` over each `crates/gw-store/migrations/*.sql` against the dockerized Postgres. Called by `wait_for_ready.sh` before pytest invocation.

When migration 0004 lands (T0-C in known-gaps plan), this script picks it up automatically.

## 3. Acceptance criteria

The E2E harness is "done" when:

1. `make e2e` builds all four images (gateway, fake_upstream, fake_web, plus the Python venv setup is one-shot via `pip install -r`), starts compose, runs all 33+ scenarios, and tears down — all in < 5 min on a dev laptop.
2. Every scenario passes against the gateway HEAD at the moment of E2E PR merge.
3. After each known-gaps wave commits, the relevant scenarios pass — `tests/e2e/scenarios/test_*.py` becomes the verification layer for gap closure.
4. `scripts/check-invariants.sh` gains `e2e_fake_no_gw_codec_dep` lint.
5. `docs/architecture.md` gains a §10 "End-to-end testing" subsection pointing here and to the plan.
6. `docs/workflow.md` unchanged (the harness exercises existing workflow; doesn't define new gateway behaviour).

## 4. Relationship to `known-gaps` work

The two efforts are **independent branches**:

- `feature/known-gaps-closure` — implements ADR-G1 … G13 fixes.
- `feature/e2e-blackbox` — builds this harness.

Either can land first. If `e2e-blackbox` lands first, `known-gaps` waves use it for verification (test files added in the same PR as the gap fix). If `known-gaps` lands first, the harness PR adds the scenario suite retroactively. The two plans do not share files.

Both should be developed in parallel by separate subagent chains. **No cross-PR coordination needed** beyond the architectural pointer in `docs/architecture.md`.

## 5. Plan reference

See [`docs/superpowers/plans/2026-05-21-e2e-blackbox.md`](../plans/2026-05-21-e2e-blackbox.md) for the wave-based implementation plan.
