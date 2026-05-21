# 2026-05-21 — E2E Black-Box Test Harness Plan

> **Source spec**: [`docs/superpowers/specs/2026-05-21-e2e-blackbox-design.md`](../specs/2026-05-21-e2e-blackbox-design.md). Read it first.
>
> **Branch**: `feature/e2e-blackbox` (create from `origin/main`). Independent of `feature/known-gaps-closure`.
>
> **Execution model**: subagent-driven, TDD-discipline-light (scenarios are themselves the tests; we cannot write a "failing scenario before infrastructure" — scenarios depend on fakes).

## Wave overview

```
Wave 0  workspace scaffold + Dockerfile + compose skeleton           [SERIAL]
Wave 1  fake_upstream_server                                          [parallel with W2]
Wave 2  fake_web_server                                               [parallel with W1]
Wave 3  Python fake_client harness + first 3 happy-path scenarios     [SERIAL after W1+W2]
Wave 4  scenario library (8 auth + 6 quota + 5 routing + 5 codec + 3 reliability)   [PARALLEL, 5 subagents per cluster]
Wave 5  invariants, docs/architecture §10, README update              [SERIAL]
```

Max parallel width: 5 (Wave 4). Critical path: W0 → (W1 ∥ W2) → W3 → W4 → W5.

## Task index

| ID | Wave | Title | Touches |
|----|------|-------|---------|
| E0-A | 0 | Workspace member crates + Dockerfile | `Cargo.toml`, `crates/e2e-fake-upstream/Cargo.toml`, `crates/e2e-fake-web/Cargo.toml`, `Dockerfile`, `tests/e2e/docker-compose.yml`, `tests/e2e/wait_for_ready.sh`, `tests/e2e/run_migrations.sh`, `Makefile` |
| E1-A | 1 | `fake_upstream_server` scaffold + scenario DSL loader | `crates/e2e-fake-upstream/src/{main,scenario,routes}.rs` |
| E1-B | 1 | Failure-injection primitives (RST, partial SSE, slow body, etc.) | `crates/e2e-fake-upstream/src/{inject,sse}.rs` |
| E2-A | 2 | `fake_web_server` scaffold + Postgres/Redis client | `crates/e2e-fake-web/src/{main,store,events}.rs` |
| E2-B | 2 | Admin HTTP API (entity, counter, balance, JWKS rotate, usage drain) | `crates/e2e-fake-web/src/{api,jwks,usage_drain}.rs` |
| E3-A | 3 | Python harness skeleton: fixtures, scenario builder, conftest | `tests/e2e/client/{conftest.py,requirements.txt,scenarios/__init__.py}` |
| E3-B | 3 | Three happy-path streaming scenarios (OpenAI/Anthropic/Gemini) | `tests/e2e/client/scenarios/test_happy_streaming.py` |
| E4-A | 4 | Auth failure scenarios (8) | `tests/e2e/client/scenarios/test_auth_failures.py` |
| E4-B | 4 | Quota scenarios (6) | `tests/e2e/client/scenarios/test_quotas.py` |
| E4-C | 4 | Routing/retry scenarios (5) | `tests/e2e/client/scenarios/test_routing_retry.py` |
| E4-D | 4 | Codec/cross-protocol scenarios (5) | `tests/e2e/client/scenarios/test_cross_protocol.py` |
| E4-E | 4 | Reliability scenarios (3) | `tests/e2e/client/scenarios/test_reliability.py` |
| E5-A | 5 | Invariants lint + docs/architecture.md §10 + Makefile polish | `scripts/check-invariants.sh`, `docs/architecture.md` |

Total: 12 tasks. Single feature branch; one PR at merge.

## Conventions

Same as the `known-gaps` plan: path-allowlisted `git add`, one subagent = one commit, three-stage review per task (implementer → spec compliance → code quality). The Wave 4 tasks are genuinely parallel — each test file is independent and the scenarios share only `conftest.py` (frozen by E3-A).

---

# Wave 0 — Scaffold

## E0-A — Workspace + Dockerfile + compose skeleton

**Files touched**:
- `Cargo.toml` (root) — extend `[workspace] members`
- `crates/e2e-fake-upstream/Cargo.toml` (new)
- `crates/e2e-fake-upstream/src/main.rs` (new, stub)
- `crates/e2e-fake-web/Cargo.toml` (new)
- `crates/e2e-fake-web/src/main.rs` (new, stub)
- `Dockerfile` (new, root; production gateway image)
- `Dockerfile.e2e-fake-upstream` (new)
- `Dockerfile.e2e-fake-web` (new)
- `tests/e2e/docker-compose.yml` (new)
- `tests/e2e/wait_for_ready.sh` (new)
- `tests/e2e/run_migrations.sh` (new)
- `Makefile` (new or extend)
- `.dockerignore` (new or extend)

**Steps**:

1. **Workspace registration**. Add both crates to root `[workspace.members]`. Confirm `cargo build --workspace` builds the two new stubs.

2. **Stub `main.rs`** for both fakes:

   ```rust
   // crates/e2e-fake-upstream/src/main.rs
   fn main() {
       eprintln!("fake_upstream_server stub — implement in E1-A");
       std::process::exit(0);
   }
   ```

   Same shape for `fake_web`. These bin exits cleanly so `cargo run` works.

3. **Production Dockerfile** (`Dockerfile` at root):

   ```dockerfile
   FROM rust:1.86-bookworm AS build
   WORKDIR /src
   COPY . .
   RUN cargo build --release -p gw-gateway

   FROM debian:bookworm-slim
   RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
   COPY --from=build /src/target/release/gw-gateway /usr/local/bin/gw-gateway
   COPY config/default.toml /etc/gw-gateway/default.toml
   ENV GW_CONFIG=/etc/gw-gateway/default.toml
   EXPOSE 8000 9090 8080
   ENTRYPOINT ["/usr/local/bin/gw-gateway"]
   ```

   This is the **production** image — no debug flags, no test hooks.

4. **Fake Dockerfiles** mirror the above with the respective `-p e2e-fake-upstream` / `-p e2e-fake-web`. Expose 7000 (web admin) and 9000 (fake upstream LLM port).

5. **docker-compose.yml**:

   ```yaml
   services:
     postgres:
       image: postgres:16-alpine
       environment:
         POSTGRES_PASSWORD: postgres
         POSTGRES_DB: gateway
       healthcheck:
         test: ["CMD-SHELL", "pg_isready -U postgres"]
         interval: 1s
         retries: 30
     redis:
       image: redis:7-alpine
       healthcheck:
         test: ["CMD", "redis-cli", "ping"]
         interval: 1s
         retries: 30
     fake-upstream:
       build:
         context: ..
         dockerfile: ../Dockerfile.e2e-fake-upstream
       ports: ["9000:9000"]
     fake-web:
       build:
         context: ..
         dockerfile: ../Dockerfile.e2e-fake-web
       depends_on:
         postgres: { condition: service_healthy }
         redis: { condition: service_healthy }
       environment:
         FAKE_WEB_PG_URL: postgres://postgres:postgres@postgres:5432/gateway
         FAKE_WEB_REDIS_URL: redis://redis:6379
       ports: ["7000:7000"]
     gateway:
       build:
         context: ..
         dockerfile: ../Dockerfile
       depends_on:
         redis: { condition: service_healthy }
         postgres: { condition: service_healthy }
         fake-web: { condition: service_started }
       environment:
         GW_REDIS__URL: redis://redis:6379
         GW_PGSQL__URL: postgres://postgres:postgres@postgres:5432/gateway
         GW_KMS__KEK_HEX: deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef
       ports: ["8000:8000", "9090:9090", "8080:8080"]
   ```

   `KEK_HEX` is a fixed 64-char hex with letters (per CLAUDE.md figment coercion caveat). All-lowercase deadbeef * 8.

6. **`wait_for_ready.sh`**:

   ```bash
   #!/usr/bin/env bash
   set -euo pipefail
   ./tests/e2e/run_migrations.sh
   for i in $(seq 1 60); do
     if curl -sf http://localhost:8080/readyz > /dev/null; then
       echo "gateway ready"
       exit 0
     fi
     sleep 1
   done
   echo "gateway did not become ready in 60s"
   docker compose -f tests/e2e/docker-compose.yml logs --tail=200 gateway
   exit 1
   ```

7. **`run_migrations.sh`**:

   ```bash
   #!/usr/bin/env bash
   set -euo pipefail
   for f in crates/gw-store/migrations/*.sql; do
     docker compose -f tests/e2e/docker-compose.yml exec -T postgres psql -U postgres -d gateway -f - < "$f"
   done
   ```

   Mark both scripts executable (`chmod +x` and commit).

8. **Makefile**:

   ```makefile
   .PHONY: e2e e2e-up e2e-down e2e-logs

   e2e:
   \tdocker compose -f tests/e2e/docker-compose.yml up -d --build
   \t./tests/e2e/wait_for_ready.sh
   \tcd tests/e2e/client && python -m pytest -v
   \tdocker compose -f tests/e2e/docker-compose.yml down -v

   e2e-up:
   \tdocker compose -f tests/e2e/docker-compose.yml up -d --build
   \t./tests/e2e/wait_for_ready.sh

   e2e-down:
   \tdocker compose -f tests/e2e/docker-compose.yml down -v

   e2e-logs:
   \tdocker compose -f tests/e2e/docker-compose.yml logs --tail=200 -f
   ```

   `\t` = literal tab.

9. **`.dockerignore`** — at minimum `target/`, `.git/`, `tests/e2e/client/.venv/`.

10. **Smoke test**: `make e2e-up` from a clean state. Confirm `/readyz` returns 200. `make e2e-down`. No pytest yet — that's E3.

11. **Commit**:

    ```
    git add Cargo.toml Cargo.lock \
            crates/e2e-fake-upstream/Cargo.toml crates/e2e-fake-upstream/src/main.rs \
            crates/e2e-fake-web/Cargo.toml crates/e2e-fake-web/src/main.rs \
            Dockerfile Dockerfile.e2e-fake-upstream Dockerfile.e2e-fake-web \
            tests/e2e/docker-compose.yml tests/e2e/wait_for_ready.sh tests/e2e/run_migrations.sh \
            Makefile .dockerignore

    git commit -m "e2e(scaffold): workspace fakes + Dockerfile + docker-compose + Makefile

    Adds the bones for the docker-based E2E harness:
    - Two new workspace members: crates/e2e-fake-{upstream,web} (stubs).
    - Production Dockerfile at root (multi-stage, distroless-ish slim).
    - Dockerfiles for the two fakes.
    - tests/e2e/docker-compose.yml with postgres+redis+fake-upstream+
      fake-web+gateway and healthcheck-gated startup order.
    - wait_for_ready.sh and run_migrations.sh entrypoint scripts.
    - Makefile with e2e/e2e-up/e2e-down/e2e-logs targets.

    make e2e-up succeeds on a clean checkout and /readyz returns 200.
    Scenario implementation lands in waves 1-4.

    Spec: docs/superpowers/specs/2026-05-21-e2e-blackbox-design.md"
    ```

---

# Wave 1 — `fake_upstream_server`

## E1-A — Scaffold + scenario DSL

**Files touched**:
- `crates/e2e-fake-upstream/src/{main,scenario,routes,server}.rs`
- `crates/e2e-fake-upstream/Cargo.toml` (add hyper, tokio, serde, serde_json)

**Steps**:

1. `Cargo.toml` deps: `hyper = { version = "1", features = ["full"] }`, `hyper-util`, `tokio = { version = "1", features = ["full"] }`, `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`, `bytes = "1"`, `gw-core = { path = "../gw-core" }` (for `ids::*` only).

2. `scenario.rs` — `Scenario` struct deserialised from JSON:

   ```rust
   #[derive(serde::Deserialize)]
   pub struct Scenario { pub routes: Vec<Route> }

   #[derive(serde::Deserialize)]
   pub struct Route {
       pub r#match: MatchSpec,
       #[serde(default)]
       pub auth_check: Option<AuthCheck>,
       pub response: ResponseSpec,
   }

   #[derive(serde::Deserialize)]
   pub struct MatchSpec {
       #[serde(default)]
       pub method: Option<String>,
       #[serde(default)]
       pub path_regex: Option<String>,
       #[serde(default)]
       pub header_eq: Option<(String, String)>,
   }

   #[derive(serde::Deserialize)]
   #[serde(tag = "kind", rename_all = "snake_case")]
   pub enum ResponseSpec {
       Static { status: u16, headers: std::collections::HashMap<String, String>, body: String, #[serde(default)] delay_ms: u64 },
       Sse { #[serde(default)] delay_ms: u64, events: Vec<SseEvent> },
       TcpResetAfterHeaders,
       TcpResetMidBody { after_bytes: usize },
       ConnectionRefusedFirstN { n: u32 },
       SlowBodyBytePerSecond { body: String, bps: u32 },
       PartialSseEvent { half_body: String },
       StatusWithRetryAfter { status: u16, retry_after_secs: u32, body: String },
   }

   #[derive(serde::Deserialize)]
   pub struct SseEvent { pub data: String, #[serde(default)] pub after_ms: u64 }
   ```

3. `main.rs` boot:
   - Read `--scenario <path>` flag (or `FAKE_UPSTREAM_SCENARIO` env var; default `/etc/fake-upstream/scenario.json`).
   - If file absent, load a built-in baseline scenario (one route returning a tiny SSE happy path) so the binary is usable out-of-the-box.
   - Bind `0.0.0.0:9000`.
   - Expose admin endpoint `POST /admin/scenario` (port 9001) — replaces the live scenario at runtime. Holds an `Arc<RwLock<Scenario>>`. Pytest writes new scenarios per test.

4. `routes.rs` — match request against each route in order; first hit wins. Per-route counter so `ConnectionRefusedFirstN` knows when to stop refusing.

5. `server.rs` — `hyper` service that for each request:
   - Reads the request fully (the gateway's content-length header tells us how much).
   - Matches scenario.
   - Dispatches to the right `ResponseSpec` handler (sub-step in E1-B).

6. **Smoke test**: launch the binary, curl `http://localhost:9000/v1/chat/completions -d '{}'`, get the default SSE happy path.

7. **Commit** with the path allowlist.

## E1-B — Failure-injection primitives

**Files touched**:
- `crates/e2e-fake-upstream/src/inject.rs` (new)
- `crates/e2e-fake-upstream/src/sse.rs` (new)
- `crates/e2e-fake-upstream/src/server.rs` (extend)

**Steps**:

1. `inject.rs` — implementations of each `ResponseSpec` variant beyond `Static`:
   - **TcpResetAfterHeaders**: write the response headers, then `shutdown(Shutdown::Both)` on the underlying `tokio::net::TcpStream`. Requires dropping out of hyper's high-level handler — accept the raw `TcpStream` via `hyper_util::server::conn` and a low-level service.
   - **TcpResetMidBody**: write headers + `after_bytes` of body, then RST.
   - **ConnectionRefusedFirstN**: an outer `Arc<AtomicU32>` counter; when `> 0`, decrement and immediately drop the accepted socket before TLS or HTTP handshake.
   - **SlowBodyBytePerSecond**: write body byte-by-byte with `tokio::time::sleep(Duration::from_secs(1) / bps)` between bytes.
   - **PartialSseEvent**: write a half-formed event without the trailing `\n\n` boundary, then close cleanly.
   - **StatusWithRetryAfter**: regular response with `Retry-After: N` header.

2. `sse.rs` — helper that writes an `SseEvent` sequence with per-event delays. Used by the `Sse` variant in E1-A and the `PartialSseEvent` variant here.

3. Per-test isolation: each scenario `POST /admin/scenario` resets the `ConnectionRefusedFirstN` counter. The admin endpoint clears all counters atomically.

4. **Tests** within `crates/e2e-fake-upstream/tests/inject.rs`: spawn the server in-process on `127.0.0.1:0`, drive each variant, assert correct wire behaviour (use a raw `TcpStream` client; not hyper, so we can observe RST).

5. **Commit**.

---

# Wave 2 — `fake_web_server`

## E2-A — Scaffold + Postgres/Redis client

**Files touched**:
- `crates/e2e-fake-web/src/{main,store,events}.rs`
- `crates/e2e-fake-web/Cargo.toml`

**Steps**:

1. Deps: `tokio`, `axum = "0.7"` (HTTP server, lighter than hyper bare), `serde`, `serde_json`, `sqlx = { version = "0.7", features = ["postgres", "runtime-tokio-rustls"] }`, `fred = { version = "9", features = ["enable-rustls"] }`, `time`, `gw-core` (path dep, for `ids::*`).

2. `store.rs` — wraps sqlx pool with helpers for the entity tables:

   ```rust
   pub struct Store { pub pg: sqlx::PgPool, pub redis: fred::clients::RedisClient }

   impl Store {
       pub async fn put_entity(&self, table: &str, id: &str, payload: serde_json::Value) -> Result<(), Error> {
           // ON CONFLICT (id) DO UPDATE SET payload = ..., version = version + 1
           sqlx::query(...).execute(&self.pg).await?;
           // Fire events:entity invalidation
           self.events_emit_invalidate(table, id).await?;
           Ok(())
       }

       pub async fn set_counter(&self, ns: &str, id: &str, field: &str, value: i64) -> Result<(), Error> {
           let key = format!("counter:{}:{}:{}", ns, id, field);
           self.redis.set::<(), _, _>(&key, value, None, None, false).await?;
           Ok(())
       }

       pub async fn set_balance(&self, user_id: &str, micros: i64) -> Result<(), Error> {
           self.set_counter("user", user_id, "balance_micro", micros).await
       }

       pub async fn xlen(&self, stream: &str) -> Result<u64, Error> {
           Ok(self.redis.xlen::<u64, _>(stream).await?)
       }
   }
   ```

3. `events.rs`:

   ```rust
   pub async fn events_emit_invalidate(&self, ns: &str, id: &str) -> Result<(), Error> {
       self.redis.xadd::<String, _, _, _, _>(
           "events:entity", false, None, "*",
           vec![("ns", ns), ("id", id)],
       ).await?;
       Ok(())
   }
   ```

4. `main.rs` — boots `Store`, starts axum router on `0.0.0.0:7000`, also spawns the background `usage:drain` loop (every 1 s reads `usage:records`, mock-computes 1 µ/token cost from each `UsageRecord`, decrements `counter:user:{id}:balance_micro` accordingly).

5. **Smoke**: `curl http://localhost:7000/health` returns OK after Postgres + Redis come up.

6. **Commit**.

## E2-B — Admin HTTP API

**Files touched**:
- `crates/e2e-fake-web/src/{api,jwks,usage_drain}.rs`

**Steps**:

1. `api.rs` — axum routes:

   ```
   POST /entity/users/:id
   POST /entity/api_keys/:id
   POST /entity/vendor_keys/:id
   POST /entity/vendors/:id
   POST /entity/model_groups/:id
   POST /entity/pricings/:model
   POST /entity/auth_configs/:id
   DELETE /entity/:table/:id

   POST /counter/:scope/:id/:field   { value: i64 }
   POST /balance/:user_id            { micros: i64 }   (shortcut)

   POST /usage/drain                 (force one drain cycle now)
   GET  /usage/stats                 { records_xlen, dlq_xlen, detailed_xlen }

   POST /jwks/rotate                 { kid: "k2" }     (returns { private_pem })
   ```

   Each entity route takes a JSON body with the entity's full payload, calls `Store::put_entity`, returns 200. The handler also fires the `events:entity` invalidation.

2. `jwks.rs` — generates a fresh RS256 keypair on rotation, stores the public PEM in the `jwks` table, returns the private PEM to the caller. Uses `rsa` + `pkcs8` crates.

3. `usage_drain.rs` — background tokio task. Every 1 s reads `XREAD usage:records BLOCK 1000 COUNT 100`; for each record decode the JSON, sum `(input + output) * markup_chain.product`, decrement `counter:user:{eid}:balance_micro`. Forces `USER_CIRCUIT` to trip on the gateway side when balance < 0.

4. **Smoke**: `curl -X POST -d '{"micros":1000000}' http://localhost:7000/balance/u1` then `redis-cli get counter:user:u1:balance_micro` → 1000000.

5. **Commit**.

---

# Wave 3 — Python harness + happy paths

## E3-A — Harness skeleton

**Files touched**:
- `tests/e2e/client/requirements.txt` (new)
- `tests/e2e/client/conftest.py` (new)
- `tests/e2e/client/scenarios/__init__.py` (new, empty)
- `tests/e2e/client/pytest.ini` (new)

**Steps**:

1. `requirements.txt`:

   ```
   openai==1.51.0
   anthropic==0.39.0
   google-generativeai==0.8.3
   pytest==8.3.3
   pytest-asyncio==0.24.0
   httpx==0.27.2
   requests==2.32.3
   ```

   Pinned versions. Bump in a dedicated PR if upstream changes.

2. `conftest.py` — pytest fixtures:

   ```python
   import os, requests, pytest
   from typing import Iterator

   @pytest.fixture(scope="session")
   def gateway_url() -> str:
       return os.environ.get("GATEWAY_URL", "http://localhost:8000")

   @pytest.fixture(scope="session")
   def fake_web_url() -> str:
       return os.environ.get("FAKE_WEB_URL", "http://localhost:7000")

   @pytest.fixture(scope="session")
   def fake_upstream_url() -> str:
       return os.environ.get("FAKE_UPSTREAM_URL", "http://localhost:9000")

   class FakeWebClient:
       def __init__(self, base): self.base = base
       def put_api_key(self, kid: str, **kw): requests.post(f"{self.base}/entity/api_keys/{kid}", json=kw).raise_for_status()
       def put_balance(self, uid: str, micros: int): requests.post(f"{self.base}/balance/{uid}", json={"micros": micros}).raise_for_status()
       def put_vendor_key(self, kid: str, **kw): requests.post(f"{self.base}/entity/vendor_keys/{kid}", json=kw).raise_for_status()
       def put_model_group(self, gid: str, **kw): requests.post(f"{self.base}/entity/model_groups/{gid}", json=kw).raise_for_status()
       def rotate_jwks(self, kid: str) -> str:
           r = requests.post(f"{self.base}/jwks/rotate", json={"kid": kid}); r.raise_for_status()
           return r.json()["private_pem"]
       def usage_stats(self): return requests.get(f"{self.base}/usage/stats").json()

   @pytest.fixture
   def fake_web(fake_web_url) -> FakeWebClient: return FakeWebClient(fake_web_url)

   class FakeUpstreamClient:
       # parallel shape: replace scenario, force connect-refused counter, etc.
       ...
   @pytest.fixture
   def fake_upstream(fake_upstream_url) -> FakeUpstreamClient: return FakeUpstreamClient(fake_upstream_url + ":9001")

   @pytest.fixture(autouse=True)
   def isolate_state(fake_web, fake_upstream):
       # Per-test: wipe entity rows for known prefixes; reset upstream scenario to baseline.
       fake_web.reset()
       fake_upstream.reset_scenario()
       yield
   ```

3. `pytest.ini`:

   ```ini
   [pytest]
   asyncio_mode = auto
   addopts = -ra --tb=short
   testpaths = scenarios
   ```

4. **Smoke**: `cd tests/e2e/client && pip install -r requirements.txt && python -m pytest --collect-only` should list 0 tests but succeed.

5. **Commit**.

## E3-B — Three happy-path scenarios

**Files touched**:
- `tests/e2e/client/scenarios/test_happy_streaming.py` (new)

**Steps**:

1. Write 3 scenarios:

   ```python
   # OpenAI client → OpenAI-compatible upstream (same-protocol passthrough)
   def test_openai_stream_happy(gateway_url, fake_web, fake_upstream):
       fake_web.put_vendor(id="v1", base_url=fake_upstream.url("/openai"))
       fake_web.put_vendor_key(kid="k1", vendor_id="v1", api_key="sk-up-1", accessible_endpoints=["openai"])
       fake_web.put_model_group(gid="free", keys=["k1"], strategy="fixed_order")
       fake_web.put_api_key(kid="ak1", user_id="u1", model_group="free")
       fake_web.put_balance("u1", 1_000_000_000)
       fake_upstream.set_scenario_openai_happy()

       import openai
       client = openai.OpenAI(api_key="ak1", base_url=f"{gateway_url}/v1",
                               default_headers={"x-api-key-id": "ak1"})
       chunks = list(client.chat.completions.create(
           model="gpt-4", messages=[{"role": "user", "content": "hi"}], stream=True,
       ))
       text = "".join(c.choices[0].delta.content or "" for c in chunks)
       assert text == "hi"
       assert fake_web.usage_stats()["records_xlen"] >= 1
   ```

   Mirror for Anthropic SDK + Gemini SDK.

2. `make e2e` runs the three tests against a freshly composed stack and they pass.

3. **Commit**.

---

# Wave 4 — Scenario library (parallel, 5 subagents)

Each task is one file under `tests/e2e/client/scenarios/`. Files are independent — different scenarios touch different entities and reset state via the `isolate_state` autouse fixture. Five subagents can run in parallel.

## E4-A — Auth failure scenarios (8 tests)

**File**: `tests/e2e/client/scenarios/test_auth_failures.py`

Scenarios:
- `test_expired_api_key_rejects_401_GW_AUTH_KEY_EXPIRED`
- `test_ip_not_in_allowlist_rejects_403_GW_AUTH_KEY_IP_NOT_ALLOWED`
- `test_hmac_required_but_missing_rejects_401_signature_missing`
- `test_hmac_required_bad_signature_rejects_401_signature_invalid`
- `test_hmac_timestamp_drift_rejects_401_timestamp_drift`
- `test_jwt_iss_mismatch_rejects_401_invalid_iss`
- `test_jwt_aud_mismatch_rejects_401_invalid_aud`
- `test_jwks_rotation_old_kid_rejected_new_accepted`

Each test:
1. Sets up entities via `fake_web`.
2. Sends a request via the relevant SDK (or raw `requests` for header-level cases that SDK won't expose).
3. Asserts HTTP status + `error.code` field in the response body.

**Commit**.

## E4-B — Quota scenarios (6 tests)

**File**: `test_quotas.py`. Scenarios per spec ADR-E1-5 §Quota. **Commit**.

## E4-C — Routing/retry scenarios (5 tests)

**File**: `test_routing_retry.py`. Scenarios per spec ADR-E1-5 §Routing. **Commit**.

## E4-D — Codec/cross-protocol scenarios (5 tests)

**File**: `test_cross_protocol.py`. Scenarios per spec ADR-E1-5 §Codec. **Commit**.

## E4-E — Reliability scenarios (3 tests)

**File**: `test_reliability.py`. Scenarios per spec ADR-E1-5 §Reliability. The DLQ test uses `fake_web` admin to corrupt the `usage:records` stream (write a wrong-type value at that key), then asserts `usage_stats()["dlq_xlen"]` grows after traffic. **Commit**.

---

# Wave 5 — Consolidation

## E5-A — Invariant + architecture doc + Makefile polish

**Files touched**:
- `scripts/check-invariants.sh` (add `e2e_fake_no_gw_codec_dep`)
- `docs/architecture.md` (new §10 End-to-end testing)
- `Makefile` (no-op if E0-A already clean)

**Steps**:

1. New invariant:

   ```bash
   echo "  check: e2e_fake_no_gw_codec_dep"
   if grep -RnE '^\s*(gw_codec|crate::gw_codec|gw-codec\s*=)' crates/e2e-fake-{upstream,web}/src crates/e2e-fake-{upstream,web}/Cargo.toml 2>/dev/null | grep -v '^\s*//'; then
     echo "FAIL: fakes must not depend on gw-codec"; exit 1
   fi
   ```

2. `docs/architecture.md` new §10:

   ```markdown
   ## 10. End-to-end testing

   `make e2e` boots a docker-compose stack (postgres + redis + fake_upstream + fake_web + gateway) and runs the pytest suite in `tests/e2e/client/scenarios/`. The harness exercises real network sockets, real Pingora hooks, and real client SDKs (openai, anthropic, google-generativeai). See [`docs/superpowers/specs/2026-05-21-e2e-blackbox-design.md`](superpowers/specs/2026-05-21-e2e-blackbox-design.md) for the three-actor model and the scenario coverage matrix.
   ```

3. Confirm `make e2e` on a clean checkout runs all 33+ scenarios end-to-end and they pass.

4. **Commit**.

---

# Dispatch checklist

Same as `known-gaps`: implementer → spec compliance reviewer → code quality reviewer per task. Reviews focus on:
- Spec compliance: does the scenario / fake behaviour match the ADR?
- Code quality: are the fakes minimal (no business logic creep)?
- Coverage: do the scenarios actually exercise the gap they claim to (e.g. E4-C's `test_first_key_connect_refused_falls_through_to_second` must trip G10's `fail_to_connect` retry, observable via `gw_retry_attempt_total{outcome=recovered}` from `/metrics`).

## End-of-branch closure

When all 12 commits land and `make e2e` passes:

1. Push `feature/e2e-blackbox` to origin.
2. Open PR against `main` with the body being §0 of the spec (the three-actor diagram + ADR list).
3. Once merged, the `known-gaps` PR (or any future PR) gains the harness as a verification surface.
