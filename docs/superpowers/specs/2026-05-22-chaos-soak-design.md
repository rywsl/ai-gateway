# Chaos Soak Test System — Design

**Date**: 2026-05-22
**Status**: Draft (awaiting user review)
**Scope**: Build a 50,000-req / 60-s soak harness that exercises the gateway under mixed protocols, mixed connection shapes, and configurable upstream chaos — designed to surface concurrency / accounting / resource / codec bugs, not just survive load.

---

## 1. Goals

### 1.1 Primary

Build a soak/chaos test system that drives the **real** gateway (docker-compose) at **833 rps × 60 s ≈ 50,000 requests**, with:

- Three client protocols (OpenAI / Anthropic / Gemini) mixed roughly equally.
- Both streaming and non-streaming responses.
- A small fraction of cross-protocol requests (`protocol_conversion=true`).
- Configurable upstream chaos: `off` (happy-path only), `light`, `heavy` — each `light/heavy` profile draws responses from a weighted table covering the existing 6 fault variants.
- A `smoke` mode (`target_rps=100, duration_s=10`) for CI.

### 1.2 Bug-finding targets

The soak is tuned to surface — not just survive — the following classes of bug:

1. **Concurrency & state machines** — routing health snapshot races, sticky / cooldown TTL math, in-flight counter drift, key-health transitions stuck.
2. **Accounting loss** — `usage:records` silently dropped without DLQ; balance double-debit / under-debit; `usage:detailed` field loss under load.
3. **Resource leaks** — tokio task leaks, upstream connection fd leaks, unbounded Moka cache growth, internal channel saturation.
4. **Codec edge cases** — SSE multi-event frames mis-parsed under chaos-induced fragmentation; cross-protocol re-render field loss; large/non-UTF-8 body panics.
5. **Error-code misclassification** — chaos injection producing a gateway `error_code` that is not in `crates/gw-core/src/error/codes.rs`.
6. **Latency degradation patterns** — p99 spikes that hint at lock contention rather than uniform load growth.

### 1.3 Non-goals

- The gateway codepath is **not** modified to support soak. The soak only observes data the gateway already emits.
- No standalone load-generator crate. The new code lives in the existing three ends: `crates/e2e-fake-upstream/`, `crates/e2e-fake-web/`, and `tests/e2e/client/`.
- This spec does not address horizontal scale-out testing, multi-gateway coordination, or chaos at the Pingora HTTP/2 layer.

---

## 2. Constraints

**C1. Gateway code zero-touch.**
The soak consumes only interfaces the gateway already exposes:
- `GET /metrics` (Prometheus catalog — existing entries only)
- `GET /livez`, `GET /readyz`, `GET /healthz/*`
- gateway stderr / `docker compose logs gateway`
- Redis streams: `usage:records`, `usage:records:dlq`, `usage:detailed`, `events:entity`
- Redis counter keys (`counter:user:{id}:*`)
- PG entity tables

Adding any new gateway metric / log / endpoint to support soak is **forbidden**. If an invariant requires observability that does not exist today, that invariant is omitted, not bolted on.

**C2. Code is sharded across the three existing ends.**
There is no fourth "runner" crate. The orchestrator is a single pytest file whose responsibility is wiring three things that already exist into a sequence.

**C3. No log whitelist.**
The log-scan invariant is intentionally scoped to **only 100%-bug signals** (panic, backtrace, assertion failed, fatal). WARN/ERROR are not scanned because the gateway emits them on normal cooldown / quota / health-degrade paths. The decision to forbid a whitelist is a feature, not an oversight — it forces the scan to be precise.

**C4. Report is AI-consumable, not human-reviewable.**
The report is JSON-tree + JSONL, every leaf carries a stable dot-path and an explicit `source` field. No markdown. No defaults. Missing data is `null` with a `reason`, never zero or imputed.

**C5. Account consistency is strict equality at eventual-consistency steady state.**
After draining for `quiescence_s=10` seconds of `XLEN` no-change (max `drain_grace_max_s=60`), the equality
`usage_records_delta + usage_records_dlq_delta == fake_upstream_total_hits`
must hold exactly. Diff ≥ 1 is a hard FAIL. This requires soak seed to disable retry (single-key model groups, no cross-vendor fallback).

**C6. No "dry-run / calibrate later" escape hatch.**
All numeric thresholds, chaos weights, and variant→error_code mappings are pinned in this spec. The plan does not include a calibration step. The only acceptable verification of "soak feature complete" is a real run of `test_soak_chaos_off_60s` and `test_soak_chaos_heavy_60s` (full 833 rps × 60 s, not smoke) whose written reports show all hard invariants `passed: true`. A smoke-mode green is **not** sufficient evidence — smoke exists for iteration on driver/seed mechanics, not for verifying the feature. Claims of completion without producing the full reports are not acceptable.

---

## 3. High-level architecture

```
                 ┌──────────────────────────────────────────────┐
                 │  tests/e2e/client/scenarios/                 │
                 │     test_chaos_soak.py    (orchestrator)     │
                 │  ──────────────────────────────────────────  │
                 │  · seed 三协议 + cross-proto entity graph    │
                 │  · POST chaos scenario JSON → fake-upstream  │
                 │  · 启动 driver (duration_s, target_rps)      │
                 │  · 等待 drain quiescence                     │
                 │  · 收 driver + fake-web /soak/report +       │
                 │    fake-upstream /admin/hits-total + /metrics│
                 │  · 跑 invariant 断言 + 日志扫描              │
                 │  · 产出 .json + .jsonl 报告                  │
                 └──────────────────────────────────────────────┘
                                  │  ┌─────────────────────────────────┐
                                  ▼  ▼
       ┌──────────────────────────────────────────────────┐
       │  tests/e2e/client/soak/driver.py  (新模块)        │
       │  ──────────────────────────────────────────────  │
       │  · ThreadPoolExecutor × N（突破 GIL）            │
       │  · 每线程一个 asyncio loop + httpx.AsyncClient  │
       │  · 速率发射器: sleep(1/target_rps) tick          │
       │  · 每 tick 按权重抽 protocol × shape × cross-pr  │
       │  · 每请求结果 → in-memory report                 │
       └──────────────────────────────────────────────────┘
                                  │
                                  ▼ HTTP × N 并发
       ┌──────────────────────────────────────────────────┐
       │       Gateway (real, docker-compose)             │
       └──────────────────────────────────────────────────┘
                                  │
                                  ▼ 上游回程
       ┌──────────────────────────────────────────────────┐
       │  crates/e2e-fake-upstream  (扩展)                │
       │  ──────────────────────────────────────────────  │
       │  · ResponseSpec::Weighted { variants } 新增      │
       │  · env FAKE_UPSTREAM_CHAOS=on/off 启动开关       │
       │  · 抽样 dispatch + 命中计数                      │
       │  · GET /admin/hits-total                         │
       │  · GET /admin/chaos-stats                        │
       └──────────────────────────────────────────────────┘

       ┌──────────────────────────────────────────────────┐
       │  crates/e2e-fake-web  (扩展)                     │
       │  ──────────────────────────────────────────────  │
       │  · POST /soak/window/start { user_ids }          │
       │  · GET  /soak/window/report?snapshot_id=...      │
       │  · 旁路消费 usage:detailed 做 spectrum 聚合      │
       └──────────────────────────────────────────────────┘
```

Data flow: orchestrator → driver concurrently hits the gateway; gateway forwards to fake-upstream which dispatches by weighted scenario (or happy-path when `CHAOS=off`); fake-web drains `usage:records` + `usage:records:dlq` in background and exposes window-delta aggregates. After driver completes, orchestrator polls fake-web until XLEN deltas are quiescent for 10 s, then collects driver report + fake-web report + fake-upstream hits + gateway `/metrics` + gateway stderr, runs invariants, writes the JSON/JSONL report.

---

## 4. End ① — fake-upstream changes

**Files touched**: `crates/e2e-fake-upstream/src/{scenario.rs, server.rs, inject.rs, main.rs}` (~200 lines net add).

### 4.1 `ResponseSpec::Weighted` DSL variant

Add to `scenario.rs::ResponseSpec`:

```rust
ResponseSpec::Weighted {
    variants: Vec<WeightedVariant>,
}

#[derive(serde::Deserialize, Clone)]
pub struct WeightedVariant {
    pub weight: u32,
    pub spec: Box<ResponseSpec>,
}
```

Wire-side JSON form:

```json
{
  "kind": "weighted",
  "variants": [
    {"weight": 90, "spec": {"kind": "sse",  "events": [...]}},
    {"weight": 3,  "spec": {"kind": "tcp_reset_mid_body", "after_bytes": 256}},
    {"weight": 2,  "spec": {"kind": "status_with_retry_after", "status": 429, "retry_after_secs": 1, "body": "..."}},
    {"weight": 2,  "spec": {"kind": "slow_body_byte_per_second", "body": "...", "bps": 4}},
    {"weight": 1,  "spec": {"kind": "partial_sse_event", "half_body": "data: {\"choices"}},
    {"weight": 1,  "spec": {"kind": "tcp_reset_after_headers"}},
    {"weight": 1,  "spec": {"kind": "static", "status": 500, "body": "{\"error\":\"oops\"}"}}
  ]
}
```

Convention: **`variants[0]` is the happy-path** — chaos-off mode falls through to it. The admin endpoint deserialiser rejects empty `variants`.

Nested `Weighted` is forbidden — admin endpoint validates each `variants[i].spec` is not itself `Weighted` before accepting the scenario.

### 4.2 Chaos enable flag

`main.rs` reads env at boot:

```rust
static CHAOS_ENABLED: AtomicBool = AtomicBool::new(false);

let on = std::env::var("FAKE_UPSTREAM_CHAOS")
    .map(|s| matches!(s.as_str(), "on" | "1" | "true"))
    .unwrap_or(false);
CHAOS_ENABLED.store(on, Ordering::SeqCst);
```

Not hot-reloadable. To switch modes, the soak orchestrator runs `docker compose up -d --build` with the env var overridden.

### 4.3 Dispatch — one new layer at the top

`server.rs::dispatch_spec` gets a single `Weighted` unwrap layer:

```rust
async fn dispatch_spec(stream: TcpStream, spec: ResponseSpec) -> std::io::Result<()> {
    let resolved = match spec {
        ResponseSpec::Weighted { variants } => {
            if variants.is_empty() {
                unreachable!("admin endpoint rejects empty variants");
            }
            if CHAOS_ENABLED.load(Ordering::Relaxed) {
                pick_weighted(&variants)  // rand::thread_rng + WeightedIndex
            } else {
                (*variants[0].spec).clone()
            }
        }
        other => other,
    };
    // existing 7 variant arms unchanged ↓
    match resolved { /* unchanged */ }
}
```

The seven existing fault arms (`Static`, `Sse`, `TcpResetAfterHeaders`, `TcpResetMidBody`, `ConnectionRefusedFirstN`, `SlowBodyBytePerSecond`, `PartialSseEvent`, `StatusWithRetryAfter`, `GzipStatic`) are not modified.

Per-request independent sampling — no per-connection or per-IP stickiness. Random source is `rand::thread_rng()`; no user-supplied seed (chaos under random + 833 rps gives sufficient sample size that variance is bounded by the central limit theorem).

### 4.4 Hit counters & admin endpoints

Two new admin endpoints, both feed Section 5 invariants:

**`GET /admin/hits-total`** — sum of all route hits + refused-connection hits since process start.

```json
{
  "routes_hit_sum": 49998,
  "refused_hit_sum": 3,
  "total": 50001,
  "started_at_iso": "2026-05-22T14:30:00Z"
}
```

`RefuseCounter` (in `inject.rs`) gains a sibling `AtomicU64 refused_hits`; `take_one()` increments it when it returns true. `routes_hit_sum` sums the existing per-route `HitCounters`.

**`GET /admin/chaos-stats`** — per-variant hit table, for chaos causal reconciliation.

```json
{
  "enabled": true,
  "variants": [
    {"index": 0, "kind": "sse",                       "hits": 45123},
    {"index": 1, "kind": "tcp_reset_mid_body",        "hits": 1502},
    {"index": 2, "kind": "status_with_retry_after",   "hits": 998},
    {"index": 3, "kind": "slow_body_byte_per_second", "hits": 1003},
    {"index": 4, "kind": "partial_sse_event",         "hits": 502},
    {"index": 5, "kind": "tcp_reset_after_headers",   "hits": 489},
    {"index": 6, "kind": "static",                    "hits": 503}
  ]
}
```

Per-variant counters live alongside the `Weighted` value inside the shared scenario `RwLock<Scenario>` — `pick_weighted` increments the chosen variant's counter under no lock (`AtomicU64::fetch_add(1, Relaxed)`). Stats reset on scenario swap (consistent with the existing `HitCounters` lifecycle).

### 4.5 Out of scope for fake-upstream

- No new fault variants (the existing 7 are enough).
- No per-route chaos override (`Weighted` is composable from existing variants).
- No seed control (random is fine at 50k samples).
- No hot reload of the `CHAOS_ENABLED` flag.

---

## 5. End ② — fake-web changes

**Files touched**: `crates/e2e-fake-web/src/{lib.rs, soak.rs (new)}` (~150 lines net add).

### 5.1 Two new endpoints

**`POST /soak/window/start`**

Request body:
```json
{ "user_ids": ["u-soak-001", "u-soak-002", ...] }
```

Records a snapshot:
- `XLEN usage:records`
- `XLEN usage:records:dlq` (treat missing key as 0)
- `XLEN usage:detailed`
- `XLEN events:entity`
- `LAST_ID` of `usage:detailed` (for `XRANGE` later)
- For each `user_id` in body: current `counter:user:{id}:balance_micro` value

Response:
```json
{ "snapshot_id": "<uuid>", "t0_ms": 1716412200000 }
```

**`GET /soak/window/report?snapshot_id=<uuid>`**

Computes deltas vs the recorded snapshot:

```json
{
  "snapshot_id": "<uuid>",
  "window_ms": 60123,
  "drain_quiescent": true,
  "drain_observation_series": [49991, 49998, 50001, 50001, 50001, 50001],
  "redis_streams": {
    "usage_records_delta":     49998,
    "usage_records_dlq_delta": 3,
    "usage_detailed_delta":    50001,
    "events_entity_delta":     0
  },
  "balance_decrement_micros": {
    "u-soak-001": 123456789,
    "u-soak-002": 122340000
  },
  "detailed_log_spectrum": {
    "by_status":      {"200": 47832, "402": 1024, "429": 856, "502": 289},
    "by_error_code":  {"GW_UPSTREAM_CONNECT_FAILED": 142, "GW_UPSTREAM_OVERLOADED": 856},
    "by_protocol":    {"open_ai": 16800, "anthropic": 16600, "gemini": 16600},
    "by_shape":       {"stream": 33500, "non_stream": 16500},
    "by_cross_proto": {"true": 5012, "false": 44989}
  }
}
```

### 5.2 Drain quiescence detection

`drain_quiescent` and `drain_observation_series` are populated by polling `XLEN usage:records` every 1 s after the orchestrator triggers report-collect; once 10 consecutive samples (`quiescence_s=10`) show no change, mark `drain_quiescent=true`. Hard ceiling at `drain_grace_max_s=60` — if no quiescence by then, return what we have with `drain_quiescent=false` and let the orchestrator FAIL the account-consistency invariant.

Implementation: the report handler blocks until quiescence (or timeout). Orchestrator side just waits on the HTTP call.

### 5.3 Detailed-log spectrum

`detailed_log_spectrum` is computed by `XRANGE usage:detailed <last_id> +` over the window. Each entry is parsed for the fields the gateway already writes (`status`, `error_code`, `protocol`, `stream`, `protocol_conversion`) and bucketed. **No new fields are introduced** — this is read-only consumption of the existing emission shape.

Missing fields in an entry: that entry contributes only to the buckets whose key it has. The report does not silently default; if `error_code` is absent the entry is bucketed under `by_error_code["__absent__"]`. This is data-source honesty (per C4).

### 5.4 Not done

- No metrics scraping (orchestrator does that directly against gateway `/metrics`).
- No log scraping (orchestrator does that via `docker compose logs`).
- No mutation of the existing drain loop.

---

## 6. End ③ — Python orchestrator + async driver

**New files**:

```
tests/e2e/client/
├── conftest.py                       (extend: soak helpers + chaos scenario builder)
├── soak/                             (new)
│   ├── __init__.py
│   ├── driver.py                     (~250 lines: concurrency engine)
│   ├── workload.py                   (~150 lines: request shape sampling + 3-protocol wire builders)
│   ├── seed.py                       (~100 lines: bulk seed users / keys / groups / pricing)
│   ├── chaos_profiles.py             (~80 lines: light + heavy weighted scenario JSON)
│   ├── invariants.py                 (~250 lines: all hard/soft invariant evaluators)
│   ├── log_scan.py                   (~60 lines: panic/backtrace/assertion/fatal grep)
│   ├── report.py                     (~200 lines: JSON tree + JSONL emit)
│   └── codes_catalog.py              (~40 lines: parse codes.rs for known error_code set)
└── scenarios/
    └── test_chaos_soak.py            (~180 lines: orchestrator, 3 test functions)
```

### 6.1 Driver architecture

```
SoakDriver.run(SoakConfig) -> SoakDriverReport
   │
   ├─→ ThreadPoolExecutor(max_workers=n_threads)
   │       each thread:
   │           loop = asyncio.new_event_loop()
   │           clients = {proto: httpx.AsyncClient(limits=Limits(max_connections=max_inflight))}
   │           sem = asyncio.Semaphore(max_inflight_per_thread)
   │
   ├─→ rate emitter (main thread):
   │       deadline = t0 + duration_s
   │       for tick in range(target_count):
   │           sleep_until(t0 + tick / target_rps)
   │           spec = sample_request_spec()           # workload.py
   │           submit(spec) → round-robin to worker queues
   │
   └─→ stop barrier: wait for each thread to finish in-flight + drain_grace_s seconds.
```

### 6.2 Soak configuration

```python
@dataclass(frozen=True)
class SoakConfig:
    duration_s: int = 60
    target_rps: int = 833
    n_threads: int = 8
    max_inflight_per_thread: int = 64
    per_request_timeout_s: int = 30
    drain_grace_s: int = 5            # in-flight grace
    drain_quiescence_s: int = 10      # fake-web XLEN no-change
    drain_grace_max_s: int = 60       # fake-web max wait
    chaos_mode: Literal["off", "light", "heavy"] = "off"
    workload: WorkloadMix = field(default_factory=WorkloadMix.default)
    n_users: int = 20

    @classmethod
    def smoke(cls) -> "SoakConfig":
        return cls(duration_s=10, target_rps=100, n_threads=2, max_inflight_per_thread=16, n_users=5)
```

### 6.3 Workload sampling

```python
@dataclass(frozen=True)
class WorkloadMix:
    protocol_weights:        dict[str, int]   # {"open_ai": 33, "anthropic": 33, "gemini": 34}
    shape_weights:           dict[str, int]   # {"stream": 50, "non_stream": 50}
    cross_protocol_pct:      int              # 10
    payload_size_weights:    dict[str, int]   # {"small": 70, "medium": 25, "large": 5}

    @classmethod
    def default(cls) -> "WorkloadMix": ...
```

Per-request, the driver constructs the wire-side request **directly with httpx** (no SDK). Pair-auth headers are set per-request from the seeded user pool. Three protocol wire builders (`build_openai_request`, `build_anthropic_request`, `build_gemini_request`), two shape modes each = 6 builders, each ~12 lines.

### 6.4 Seed strategy (`seed.py`)

A single `seed_soak(fake_web, n_users=20) -> SoakSeedHandles` call sets up:

- **N=20 users** with `BALANCE_LARGE = 1e12` micros (well over 50k requests at any tier).
- **20 api_keys** (one per user), pair-auth ready.
- **3 vendors** (one per protocol family).
- **3 vendor_keys**: each with `protocol_conversion=false`, `accessible_endpoints=[<protocol>]` — same-protocol passthrough.
- **1 cross-protocol vendor_key**: `accessible_endpoints=["anthropic"]`, `protocol_conversion=true` — for the cross-proto sample.
- **4 model_groups**: `soak-open_ai`, `soak-anthropic`, `soak-gemini`, `soak-cross`. **All use `strategy="fixed_order"` + `cross_vendor_fallback=false`** so retry is disabled (C5).
- **3 pricings**: one per model name.

> **2026-05-25 note**: Multi-key `round_robin` was attempted but reverted. The gateway's `FailoverExecutor` (gateway.rs:520–549) classifies 429/5xx pre-body as retryable and retries against the next cached candidate key; with multiple keys per group this produces implicit retries, violating C5 (`1 request = 1 upstream call`) and breaking `records_equals_upstream_hits`. Seed remains at **1 vendor key per model group** with `strategy="fixed_order"` + `cross_vendor_fallback=false`. The single-key 512 max in-flight cap is a structural capacity limit at 833 rps. Resolving it requires either a gateway-side change (disable response-failover for multi-key configurations) or accepting the current capacity ceiling.

Returns user/key handles for the driver to round-robin.

### 6.5 Chaos profiles (`chaos_profiles.py`)

Two well-known weighted scenario JSON objects exported as Python dicts:

- `LIGHT_PROFILE`: weights roughly 96 % happy / 4 % faults, distributed evenly across the 6 fault variants.
- `HEAVY_PROFILE`: roughly 80 % happy / 20 % faults, weighted toward TCP reset + retry-after to hit cooldown/health paths most.

Exact weights are pinned in §11.1 of this spec; `chaos_profiles.py` is the code-side mirror, not a tuning surface. The orchestrator POSTs the chosen profile to `fake_upstream:/admin/scenario` after seeding and before driver start.

### 6.6 Orchestrator test functions

```python
@pytest.mark.soak  # excluded by default, run with `pytest -m soak`
def test_soak_smoke(...):
    """CI variant: target_rps=100, duration_s=10, chaos=off. Validates wiring."""

@pytest.mark.soak
def test_soak_chaos_off_60s(...):
    """Local/nightly: target_rps=833, duration_s=60, chaos=off. Baseline."""

@pytest.mark.soak
def test_soak_chaos_heavy_60s(...):
    """Local/nightly: same but FAKE_UPSTREAM_CHAOS=on + HEAVY_PROFILE. Bug-finding."""
```

Each function:
1. `seed_soak(fake_web, n_users=...)`.
2. `fake_upstream.set_scenario(profile)` (for chaos modes).
3. `fake_web.soak_window_start(user_ids=[...])` → `snapshot_id`.
4. `SoakDriver(cfg).run()` → `SoakDriverReport`.
5. `fake_web.soak_window_report(snapshot_id)` → `WindowReport` (blocks on quiescence).
6. `fake_upstream.hits_total()` + `fake_upstream.chaos_stats()`.
7. Collect `/metrics` snapshot.
8. Scan gateway logs (`docker compose logs gateway --since=<t0_iso>`) for panic/backtrace/assertion/fatal.
9. `evaluate_invariants(...)` → `InvariantResults`.
10. `write_reports(.json, .jsonl)`.
11. `pytest.fail(verdict.first_failure_path)` if any hard invariant failed.

### 6.7 pytest config

`tests/e2e/client/pytest.ini` gets:

```ini
[pytest]
markers =
    soak: long-running soak/chaos tests, excluded from default `pytest`
addopts = -m "not soak"
```

`make e2e-soak` target added to Makefile: brings up the stack with `FAKE_UPSTREAM_CHAOS=on`, runs `pytest -m soak`, tears down.

---

## 7. Invariants

All invariants below have a stable dot-path under `results.invariants.<group>.<name>` in the report.

### 7.1 Hard invariants — any FAIL → overall FAIL

#### 7.1.1 `process_alive`

| Name | Source | Threshold |
|---|---|---|
| `livez_200_throughout` | driver `/livez` probe every 10 s during driver run | every probe == 200 |
| `readyz_200_throughout` | driver `/readyz` probe every 10 s | every probe == 200 |
| `no_container_restart` | `docker compose ps gateway --format json` before/after | RestartCount delta == 0, State == running |
| `no_panic` | `docker compose logs gateway --since=t0 --until=t1+grace` | zero matches against: `panicked at`, `RUST_BACKTRACE`, `thread '.*' panicked`, `assertion failed`, `\bfatal\b` (case-insensitive) |

#### 7.1.2 `account_consistency`

| Name | Source | Threshold |
|---|---|---|
| `records_equals_upstream_hits` | fake-web window report + fake-upstream `/admin/hits-total` | `records_delta + dlq_delta == hits_total` (strict equality, at quiescent steady state) |
| `balance_monotonic_decreasing` | fake-web window report `balance_decrement_micros` | every `decrement >= 0` (no balance increased mid-soak) |
| `drain_quiescence_reached` | fake-web window report `drain_quiescent` | true (within `drain_grace_max_s`) |

#### 7.1.3 `resource_release`

| Name | Source | Threshold |
|---|---|---|
| `all_in_flight_gauges_zero` | gateway `/metrics`, regex `^gw_.*in_flight.*$` | every series value == 0, post-drain |
| `rss_growth_under_100mib` | `docker stats --no-stream gateway` before/after | `Δ rss < 100 MiB` |

#### 7.1.4 `error_code_compliance`

| Name | Source | Threshold |
|---|---|---|
| `all_codes_in_catalog` | driver `by_error_code` ∪ fake-web `detailed_log_spectrum.by_error_code` vs `codes.rs` regex extraction | observed ⊆ catalog |

Catalog source: `grep -hoE '^pub const (GW_[A-Z_]+):' crates/gw-core/src/error/codes.rs | sed -E 's/^pub const //; s/:$//' | sort -u`, captured once at orchestrator startup, with the git SHA recorded in the report. As of this spec there are ~50 `GW_*` codes — the actual count is whatever the parse returns at run time.

#### 7.1.5 `chaos_causal_reconciliation` (chaos modes only — skipped under `off`)

For each `Weighted` variant index `i` whose `kind != sse|static-200` (i.e., a fault variant):

| Name | Source | Threshold |
|---|---|---|
| `<variant_kind>_produces_mapped_errors` | fake-upstream `/admin/chaos-stats` × variant → expected error_code mapping (codified in `invariants.py::VARIANT_ERROR_CODE_MAP`) | `count(driver.by_error_code[mapped_code]) >= floor(variant.hits * 0.7)` |

The variant→error_code mapping is part of this spec (Appendix A) and is validated against `crates/gw-core/src/error/codes.rs` at orchestrator startup — any mapping pointing at a code not in the catalog fails an internal sanity check before the soak even runs.

#### 7.1.6 `min_completion_rate`

| Name | Source | Threshold |
|---|---|---|
| `at_least_half_completed` | driver report `requests_completed` | `>= target_count * 0.5` |

A driver that pushed fewer than half the target shape is not exercising the gateway enough for the rest of the invariants to mean anything — this catches "test infrastructure broken" early.

### 7.2 Soft warnings — never FAIL, recorded as `status: "ok" | "warn"`

| Item | off threshold | heavy threshold |
|---|---|---|
| `success_rate_2xx_pct` | ≥ 99.9 | ≥ 80 |
| `latency_p99_ms` | < 200 | < 500 |
| `completion_rate_pct` | ≥ 90 | ≥ 60 |
| `thread_cpu_max_pct` | < 80 | < 80 |
| `chaos_variant_hit_deviation` | n/a | ≤ ±25 % (actual hit % vs declared weight %) |
| `records_to_2xx_ratio` | ≤ 1.10 | ≤ 1.10 (hint at double-emit) |

### 7.3 Log scan — narrow, no whitelist

The `no_panic` invariant in §7.1.1 is the sole log-based check. It matches **only** 100%-bug signals (panic / backtrace / assertion / fatal). WARN and ERROR are deliberately not scanned (gateway emits them on normal cooldown / quota / health paths). This is enforced by C3.

---

## 8. Report format

Two files per soak run, written to `tests/e2e/client/.soak_report/<utc-ts>-<scenario>.{json,jsonl}`. **No markdown.**

### 8.1 `<ts>-<scenario>.json` — full nested tree

Structure (illustrative — actual leaves are exhaustive per §7):

```json
{
  "soak_run": {
    "id": "2026-05-22T14:30:00Z-chaos-heavy-60s",
    "config": {
      "duration_s": 60,
      "target_rps": 833,
      "chaos_mode": "heavy",
      "chaos_profile_name": "HEAVY_PROFILE",
      "n_threads": 8,
      "max_inflight_per_thread": 64,
      "n_users": 20,
      "git_sha": "abc1234",
      "gateway_image_sha": "sha256:..."
    },
    "timeline": {
      "t0_iso": "2026-05-22T14:30:00Z",
      "driver_t1_iso": "2026-05-22T14:31:00Z",
      "drain_quiescent_at_iso": "2026-05-22T14:31:13Z",
      "drain_wall_ms": 13412
    },
    "results": {
      "invariants": {
        "process_alive": {
          "livez_200_throughout": {
            "path": "results.invariants.process_alive.livez_200_throughout",
            "kind": "hard",
            "passed": true,
            "source": "driver.livez_probe",
            "raw": { "probes": 8, "non_200_count": 0 }
          },
          "no_panic": {
            "path": "results.invariants.process_alive.no_panic",
            "kind": "hard",
            "passed": false,
            "source": "docker.compose_logs.gateway",
            "raw": {
              "scan_pattern": "panicked at|RUST_BACKTRACE|thread '.*' panicked|assertion failed|\\bfatal\\b",
              "matched_lines": [
                "2026-05-22T14:30:42.123Z gateway thread 'tokio-runtime-worker' panicked at 'index out of bounds...'"
              ],
              "match_count": 1
            }
          }
        },
        "account_consistency": {
          "records_equals_upstream_hits": {
            "path": "results.invariants.account_consistency.records_equals_upstream_hits",
            "kind": "hard",
            "passed": false,
            "source": "fake_web:/soak/window/report + fake_upstream:/admin/hits-total",
            "raw": {
              "records_delta": 49997,
              "dlq_delta": 3,
              "upstream_routes_hit_sum": 49998,
              "upstream_refused_hit_sum": 3,
              "upstream_hits_total": 50001,
              "diff": 1,
              "quiescent_for_s": 10.2,
              "drain_observation_series": [49991, 49997, 50000, 50000, 50000, 50000]
            },
            "threshold": "=="
          }
        },
        "resource_release": { ... },
        "error_code_compliance": { ... },
        "chaos_causal_reconciliation": {
          "per_variant": {
            "tcp_reset_mid_body": {
              "path": "results.invariants.chaos_causal_reconciliation.per_variant.tcp_reset_mid_body",
              "kind": "hard",
              "passed": true,
              "source": "fake_upstream:/admin/chaos-stats + driver.by_error_code",
              "raw": {
                "variant_hits": 1502,
                "mapped_error_codes": ["GW_UPSTREAM_CONNECT_FAILED", "GW_UPSTREAM_CANCELLED"],
                "matched_error_count": 1467,
                "ratio": 0.977,
                "min_ratio": 0.7
              }
            }
          }
        },
        "min_completion_rate": { ... }
      },
      "soft_warnings": {
        "success_rate_2xx_pct": {
          "path": "results.soft_warnings.success_rate_2xx_pct",
          "kind": "soft",
          "status": "warn",
          "source": "driver.by_status",
          "raw": { "actual": 78.3, "threshold": 80.0, "delta": -1.7 }
        }
      },
      "spectra": {
        "by_status":      { "source": "driver + fake_web", "raw": {...}, "cross_check_consistent": true },
        "by_error_code":  { "source": "driver + fake_web", "raw": {...}, "cross_check_consistent": true },
        "by_protocol":    { ... },
        "by_shape":       { ... },
        "by_cross_proto": { ... },
        "latency_ms":     { "source": "driver", "raw": {"p50": 23.1, "p90": 89.4, "p99": 412.0, "max": 2891.0} }
      }
    },
    "verdict": {
      "hard_invariants_total": 14,
      "hard_invariants_passed": 12,
      "hard_invariants_failed": 2,
      "first_failure_path": "results.invariants.process_alive.no_panic",
      "all_failures": [
        "results.invariants.process_alive.no_panic",
        "results.invariants.account_consistency.records_equals_upstream_hits"
      ],
      "overall": "FAIL"
    }
  }
}
```

### 8.2 `<ts>-<scenario>.jsonl` — one line per invariant

Each line is a complete, self-describing JSON object — designed for `jq -c` / streaming consumers / AI agents that grep by `path`:

```jsonl
{"path":"results.config","data":{"duration_s":60,...}}
{"path":"results.invariants.process_alive.livez_200_throughout","kind":"hard","passed":true,"source":"driver.livez_probe","raw":{"probes":8,"non_200_count":0}}
{"path":"results.invariants.process_alive.no_panic","kind":"hard","passed":false,"source":"docker.compose_logs.gateway","raw":{"matched_lines":["..."],"match_count":1}}
{"path":"results.invariants.account_consistency.records_equals_upstream_hits","kind":"hard","passed":false,"source":"fake_web + fake_upstream","raw":{"records_delta":49997,"dlq_delta":3,"upstream_hits_total":50001,"diff":1}}
...
{"path":"results.spectra.by_error_code","data":{"GW_UPSTREAM_CONNECT_FAILED":142,"GW_UPSTREAM_OVERLOADED":856,"GW_UPSTREAM_TIMEOUT":1003,"GW_UPSTREAM_5XX":503}}
{"path":"results.verdict","overall":"FAIL","first_failure_path":"results.invariants.process_alive.no_panic","all_failures":["results.invariants.process_alive.no_panic","results.invariants.account_consistency.records_equals_upstream_hits"]}
```

### 8.3 Truthfulness rules (C4)

- Every leaf has `source` — never null, never inferred. The `source` string names a concrete endpoint and/or method (e.g. `fake_web:/soak/window/report`, `docker.compose_logs.gateway`).
- `raw` is the raw observation, not a summary. If the gateway returned a numeric counter, `raw` contains that counter — not a re-derived statistic.
- Data unreachable → leaf is `passed: null` (hard) / `status: null` (soft) + `raw: {"reason": "<concrete why>"}`. Verdict counts this as FAIL.
- All timestamps ISO-8601 UTC.
- `git_sha` (current `HEAD` of the soak run) and `gateway_image_sha` (resolved at boot from `docker compose ps gateway --format json`) are captured in `config` for reproducibility.

### 8.4 Dot-path stability

The paths under `results.invariants.<group>.<name>` are fixed by §7. They do not change run-to-run. AI consumers can hard-code these paths in their analysis logic. New invariants always go under new names; never rename or move an existing one.

---

## 9. docker-compose / Makefile changes

`tests/e2e/docker-compose.yml`:

- `fake-upstream` service gains `FAKE_UPSTREAM_CHAOS` env var (default `off`). The soak Makefile target overrides to `on`.
- `fake-web` service unchanged (new endpoints are additive).
- `gateway` service unchanged.

`Makefile`:

```makefile
e2e-soak:
	FAKE_UPSTREAM_CHAOS=on docker compose -f tests/e2e/docker-compose.yml up -d --build
	./tests/e2e/wait_for_ready.sh
	cd tests/e2e/client && .venv/bin/python3 -m pytest -m soak -v
	docker compose -f tests/e2e/docker-compose.yml down -v
```

`make e2e` (default CI target) excludes soak tests automatically via `pytest.ini` `addopts = -m "not soak"`. Smoke variant is run with `make e2e-soak SOAK_SMOKE=1` (sets a pytest env var the orchestrator reads to swap in `SoakConfig.smoke()`).

---

## 10. Appendix A — variant→error_code mapping (for §7.1.5)

This table is **pinned** — every `GW_*` code listed here is grep-verifiable in `crates/gw-core/src/error/codes.rs` at this spec's commit and is what the default classifier (`crates/gw-core/src/error/classify_default.rs`) emits for the corresponding upstream condition. `HTTP_STATUS_*` and `CLIENT_SIDE_FAILURE` are soak-driver observation buckets, not gateway error codes. The orchestrator validates the `GW_*` subset against `codes.rs` at startup (per C6, no calibration phase: if a `GW_*` code listed here is absent from `codes.rs`, the soak fails immediately, before any traffic).

| Fault variant | Expected gateway error codes (any-of match) | Routing classifier hop |
|---|---|---|
| `tcp_reset_after_headers` | `GW_UPSTREAM_CONNECT_FAILED`, `GW_UPSTREAM_CANCELLED`, `CLIENT_SIDE_FAILURE` | Pingora upstream-side connection-error path (`crates/gw-proxy/src/gateway.rs:311`), or the client sees the torn response as a transport failure before an error envelope |
| `tcp_reset_mid_body` | `GW_UPSTREAM_CONNECT_FAILED`, `GW_UPSTREAM_CANCELLED`, `CLIENT_SIDE_FAILURE` | Body torn → connection error / stream-cancel, or client-side transport failure before envelope extraction |
| `connection_refused_first_n` | `GW_UPSTREAM_CONNECT_FAILED` | Accept-time RST → connect failure |
| `slow_body_byte_per_second` | `GW_UPSTREAM_TIMEOUT`, `CLIENT_SIDE_FAILURE` | read-timeout / stream-total-timeout trip, or driver timeout before a gateway envelope is read |
| `partial_sse_event` | `GW_UPSTREAM_CONNECT_FAILED`, `GW_UPSTREAM_TIMEOUT`, `GW_UPSTREAM_CANCELLED`, `CLIENT_SIDE_FAILURE` | Torn event with clean FIN — classified into one of these depending on whether bytes arrived before tear |
| `status_with_retry_after` (status=429) | `GW_UPSTREAM_OVERLOADED`, `HTTP_STATUS_429` | `classify_default.rs:93-106` — 429 → `RateLimitedUpstream` → `GW_UPSTREAM_OVERLOADED`; pass-through 429s are counted by observed HTTP status |
| `static` (status=502, used as the fault example) | `GW_UPSTREAM_5XX`, `HTTP_STATUS_502` | `classify_default.rs:117-126` — 500..=599 → `UpstreamTransient` → `GW_UPSTREAM_5XX`; pass-through 502s are counted by observed HTTP status |

**The "any-of" semantics**: a variant passes its causal-reconciliation invariant if the sum of `driver.by_error_code[c]` over `c ∈ expected_codes` meets the 0.7 ratio threshold (§7.1.5). One variant cannot map onto a single code because the upstream-error path can route into multiple classes depending on byte timing — but the union is pinned, not vague.

**Doc sync obligation (CLAUDE.md §Sync rule)**: when `codes.rs` adds or removes a `GW_UPSTREAM_*` code, or `classify_default.rs` changes a status→class mapping, this appendix updates in the same commit. The orchestrator's startup validation enforces it loudly.

---

## 11. Appendix B — pinned numeric values

Per C6 (no calibration phase), every numeric value the soak depends on is fixed here. Reviewers/implementers must change this spec to change any value — there is no run-time "tune after observation" path.

### 11.1 Chaos profile weights (`chaos_profiles.py`)

**`LIGHT_PROFILE`** — 96 % happy, 4 % faults distributed evenly across the 6 fault variants:

| index | kind | weight |
|---|---|---|
| 0 | `sse` (happy) | 96 |
| 1 | `tcp_reset_mid_body` (`after_bytes=256`) | 1 |
| 2 | `status_with_retry_after` (`status=429, retry_after_secs=1`) | 1 |
| 3 | `slow_body_byte_per_second` (`bps=4`) | 1 |
| 4 | `partial_sse_event` | 1 |
| 5 | `tcp_reset_after_headers` | 0 |
| 6 | `static` (`status=502`) | 0 |

(Weights of 0 are intentional: `light` mode focuses on partial/timeout failures, leaving harder connection-class failures to `heavy`.)

**`HEAVY_PROFILE`** — 80 % happy, 20 % faults, weighted toward connection-class and rate-limit-class faults:

| index | kind | weight |
|---|---|---|
| 0 | `sse` (happy) | 80 |
| 1 | `tcp_reset_mid_body` (`after_bytes=256`) | 5 |
| 2 | `status_with_retry_after` (`status=429, retry_after_secs=1`) | 5 |
| 3 | `slow_body_byte_per_second` (`bps=4`) | 3 |
| 4 | `partial_sse_event` | 2 |
| 5 | `tcp_reset_after_headers` | 3 |
| 6 | `static` (`status=502`) | 2 |

Both profiles use the same happy `sse` response (the existing default 2-event "hi" + "[DONE]" frame, see `crates/e2e-fake-upstream/src/scenario.rs::default_scenario`).

`connection_refused_first_n` is intentionally **not** in either weighted set — it is enforced at accept time (`server.rs:76-80`) and the refuse counter cannot be re-armed mid-window via the `Weighted` dispatch path. We get the same coverage from `tcp_reset_after_headers`, which exercises the same `GW_UPSTREAM_CONNECT_FAILED` path.

### 11.2 Soft warning thresholds (§7.2)

Pinned for the v1 release of this spec:

| Item | `off` threshold | `heavy` threshold |
|---|---|---|
| `success_rate_2xx_pct` | 99.9 | 80.0 |
| `latency_p99_ms` | 200 | 500 |
| `completion_rate_pct` | 90 | 60 |
| `thread_cpu_max_pct` | 80 | 80 |
| `chaos_variant_hit_deviation_pct` | n/a | 25 |
| `records_to_2xx_ratio_max` | 1.10 | 1.30 |

> **2026-05-25 update**: heavy `records_to_2xx_ratio_max` raised from 1.10 to 1.30. Under heavy chaos, upstream-attempted-but-failed requests (429, 502) legitimately emit billing records, and streaming requests where the gateway billed but the client timed out mid-stream also contribute. Observed ratio was 1.171; 1.30 provides headroom without masking real double-emit bugs.

(`light` mode is not separately gated — its soft warnings interpolate: 2xx ≥ 95 %, p99 < 300 ms, completion ≥ 80 %. Implementer can put these in a single `SOFT_THRESHOLDS` dict keyed by `chaos_mode`.)

### 11.3 SoakConfig defaults (full)

| Field | Full | Smoke |
|---|---|---|
| `duration_s` | 60 | 10 |
| `target_rps` | 833 | 100 |
| `n_threads` | 8 | 2 |
| `max_inflight_per_thread` | 64 | 16 |
| `per_request_timeout_s` | 30 | 30 |
| `drain_grace_s` | 5 | 5 |
| `drain_quiescence_s` | 10 | 5 |
| `drain_grace_max_s` | 60 | 30 |
| `n_users` | 20 | 5 |

### 11.4 What is NOT in CI

`make e2e-soak` is **not** wired into any default CI flow. The smoke variant exists for fast local iteration on driver/seed mechanics only. Per C6, smoke is **not** the verification signal — the verification signal is two reports from the full `chaos_off_60s` and `chaos_heavy_60s` test functions showing all hard invariants `passed: true`.

---

## 12. Implementation order (for the plan)

A separate plan file (`docs/superpowers/plans/2026-05-22-chaos-soak.md`) will be written next via the writing-plans skill. The expected high-level sequence:

1. fake-upstream: `Weighted` variant + dispatch unwrap + `CHAOS_ENABLED` + hit counters + 2 admin endpoints.
2. fake-web: `/soak/window/start` + `/soak/window/report` + drain-quiescence polling + detailed spectrum.
3. Python: `soak/seed.py` + `soak/chaos_profiles.py` (LIGHT/HEAVY pinned per §11.1) + `soak/workload.py` + `soak/codes_catalog.py`.
4. Python: `soak/driver.py` (concurrency engine).
5. Python: `soak/invariants.py` (hard invariants + Appendix A mapping validated at startup) + `soak/log_scan.py` + `soak/report.py`.
6. Python: `scenarios/test_chaos_soak.py` (orchestrator), Makefile `e2e-soak`, `pytest.ini` marker.
7. Doc sync: update `CLAUDE.md` capability surface with the soak harness; update `docs/architecture.md` if any new persistence keys land (currently none planned).

### 12.1 Verification gate (terminal step before claiming "feature complete")

Per **C6**, implementation is not complete until both:

- `pytest -m soak tests/e2e/client/scenarios/test_chaos_soak.py::test_soak_chaos_off_60s` produces a report at `tests/e2e/client/.soak_report/<ts>-chaos-off-60s.json` whose `results.verdict.overall == "PASS"`.
- `pytest -m soak tests/e2e/client/scenarios/test_chaos_soak.py::test_soak_chaos_heavy_60s` produces a report at `tests/e2e/client/.soak_report/<ts>-chaos-heavy-60s.json` whose `results.verdict.overall == "PASS"`.

Both reports must be produced by an actual 833 rps × 60 s execution. The `test_soak_smoke` green light is **explicitly not sufficient** — it does not exercise the soak's traffic profile. Verifying with smoke and claiming completion is contrary to the spec and will be rejected on review.

If either full run produces hard-invariant FAILs, the diagnosis path is:

1. Read `results.verdict.first_failure_path`.
2. `jq '<that path>' < <report>.json` to get the leaf's `raw` block.
3. The `source` field names exactly where the data came from — investigate the data source's emission (fake-web log, fake-upstream log, gateway stderr, `/metrics` series, etc.), not the soak runner.
4. The fail is either (a) a real bug in the gateway under chaos load — file an issue, do not "fix" the spec or the soak to mask it; or (b) a soak-side bug — fix the soak.

The distinction in step 4 is the most important judgement call: if `records_equals_upstream_hits` fails with `diff=1`, that is a real bug (one usage record lost), regardless of how rare it appears. Do not relax the invariant.
