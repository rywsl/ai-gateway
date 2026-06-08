# 2026-05-27 — Feature Parity Split Contracts

> **Status**: 契约变更记录(active)
> **日期**: 2026-05-27
> **触发来源**: Feature parity direct plan Task 8（Split items only）
> **范围**: Rust routing/config implementation + Go contract update
>
> **本文件作用**: 汇总 2026-05-27 feature parity 中所有 **Split-type** 变更，供 Go 管理面 / 计费面 / 聚合面消费。每条改动都记录 **Go 侧职责 + 契约 shape + Rust 证据落点**，并明确哪些仍处于 **Awaiting Go-side implementation**。

---

## 0. 变更索引

| # | Split item | Go 侧职责 | 契约落点 | Rust 证据 |
|---|---|---|---|---|
| 1 | `/v1/models` proxy | 提供模型聚合 API，被 Rust 反向代理 | HTTP only（无 Redis / PgSQL 契约新增） | `crates/gw-proxy/src/filters/request.rs`, `crates/gw-proxy/src/gateway.rs` |
| 2 | `usage:records.attempted_keys` | settler / drainer 接受并消费候选链字段 | `usage:records` stream `payload.attempted_keys[]` | `crates/gw-ledger/src/usage.rs`, `crates/gw-proxy/src/billing_lifecycle.rs` |
| 3 | `events:entity op=auto_ban_candidate` | 消费自动封禁候选事件，决定是否写回实体状态 | `events:entity` stream fields | `crates/gw-store/src/redis_client.rs`, `crates/gw-health/src/store.rs` |
| 4 | `/healthz/active-requests` | 可选跨实例聚合每实例 active request 计数 | HTTP only（无 Redis / PgSQL 契约新增） | `crates/gw-gateway/src/health.rs` |
| 5 | Scheduling presets | Go 配置 preset，Rust 按 multi-dim preset 名称执行 | `model_groups.payload.strategy` | `crates/gw-core/src/entity.rs`, `crates/gw-routing/src/pipeline/multi_dim.rs`, `crates/gw-routing/src/strategy/set.rs` |

---

## 1. `/v1/models` reverse-proxy to Go aggregation API

**Status: Awaiting Go-side implementation**

### 1.1 Contract summary

- Rust 已支持对 **已通过业务鉴权** 的 `GET /v1/models` 与 `GET /v1/models/{model_id}` 请求走 Go control-plane 反向代理；
- Go 侧需要提供与 OpenAI Models surface 兼容的聚合查询端点；
- 此路径 **不是** `/admin/*` 管理 token surface：它沿用业务侧 `Bearer JWT` / `x-api-key` 鉴权与 scoped IP checks，然后仅通过 `ctx.mgmt = true` 切到 `admin_upstream_url`。

### 1.2 Rust evidence

- Path classifier: `crates/gw-proxy/src/filters/request.rs:46-60` `is_models_proxy_request()`
- Business auth 完成后切 mgmt proxy: `crates/gw-proxy/src/filters/request.rs:439-442`
- Counter/intent: `crates/gw-gateway/src/metrics_catalog.rs:91-93` (`gw_models_proxy_total`)
- Reverse proxy target: `crates/gw-proxy/src/gateway.rs:389-410` (`ctx.mgmt` → `admin_upstream_url`)

### 1.3 Cross-process touchpoints

- **Redis stream key**: none
  - 证据: 该流量仅通过 `ctx.mgmt` 切换 upstream peer，未写 Redis；见 `crates/gw-proxy/src/filters/request.rs:439-442` 与 `crates/gw-proxy/src/gateway.rs:389-410`
- **PgSQL column**: none
  - 证据: Rust 端仅做请求鉴权与转发；未读取或写入新的 PgSQL contract 字段
- **Payload field / HTTP shape**: request path only
  - `GET /v1/models`
  - `GET /v1/models/{model_id}` where `{model_id}` must be non-empty and contain no extra `/`
  - 证据: `crates/gw-proxy/src/filters/request.rs:46-60`

### 1.4 Go-side requirement

- Go control plane must expose the upstream endpoint behind `admin.upstream_url` for:
  - collection lookup: `GET /v1/models`
  - item lookup: `GET /v1/models/{model_id}`
- Returned payload should be the OpenAI-compatible models list / model object that callers already expect from the public API.

---

## 2. `usage:records.payload.attempted_keys` candidate chain

**Status: Awaiting Go-side implementation**

### 2.1 Contract summary

- `UsageRecord` 新增可选字段 `attempted_keys`；
- 该字段用于把一次请求内的候选 key / retry 链显式发给 Go settler / drainer；
- 字段是 **additive + backward-compatible**：缺失时等价于 `null` / omitted，Go consumer 必须接受无该字段的老记录。

### 2.2 Rust evidence

- Schema definition: `crates/gw-ledger/src/usage.rs:14-21`, `53-112`
- Optional omission: `crates/gw-ledger/src/usage.rs:88-89` (`skip_serializing_if = "Option::is_none"`)
- Reconstruction logic: `crates/gw-proxy/src/billing_lifecycle.rs:37-92`
- Logging path emission: `crates/gw-proxy/src/filters/logging.rs:250`

### 2.3 Cross-process touchpoints

- **Redis stream key**: `usage:records`
  - 证据: `crates/gw-ledger/src/usage.rs:1-5`（documented as the record emitted to `usage:records`）
- **PgSQL column**: none directly in Rust contract
  - Go may persist into its billing DB, but Rust side adds no new PgSQL schema field here
- **Payload field**: `payload.attempted_keys`
  - type: `Option<Vec<AttemptedKey>>`
  - omitted when `None`
  - 证据: `crates/gw-ledger/src/usage.rs:88-89`

### 2.4 Exact JSON shape

```json
"attempted_keys": [
  {
    "vendor_key_id": "vk_xxx",
    "vendor": "v_openai",
    "model": "gpt-4o",
    "error_code": "GW_UPSTREAM_CONNECT_FAILED",
    "attempt_index": 0
  }
]
```

Field types:

| field | type | source |
|---|---|---|
| `vendor_key_id` | `string` | `AttemptedKey.vendor_key_id` |
| `vendor` | `string` | `AttemptedKey.vendor` |
| `model` | `string` | `AttemptedKey.model` |
| `error_code` | `string \| null` | `AttemptedKey.error_code` |
| `attempt_index` | `u32` | `AttemptedKey.attempt_index` |

### 2.5 Go-side requirement

- `usage:records` consumer / DLQ drainer must parse `attempted_keys` when present;
- absence of the field must remain valid input;
- Go can use the chain for settle audit, retry attribution, candidate quality analysis, or reconciliation decisions, but Rust does not require any immediate write-back response.

---

## 3. `events:entity` auto-ban candidate report

**Status: Awaiting Go-side implementation**

### 3.1 Contract summary

- Rust health path now emits a reverse event into `events:entity` when a vendor key crosses the configured consecutive-failure threshold;
- event is advisory only: Rust does **not** auto-disable the key itself;
- Go consumes the event and decides whether to persist a real `vendor_key` status change, admin action, or ignore / debounce further.

### 3.2 Rust evidence

- Health callback interface: `crates/gw-health/src/store.rs:15-17`
- Threshold/debounce config: `crates/gw-health/src/store.rs:27-31`, `57-69`
- Redis XADD implementation: `crates/gw-store/src/redis_client.rs:32-63`

### 3.3 Cross-process touchpoints

- **Redis stream key**: `events:entity`
  - 证据: `crates/gw-store/src/redis_client.rs:9`, `52-54`
- **PgSQL column**: none in the emitted event itself
  - Go-side follow-up may update `vendor_keys.payload.status` / `version`, but the emitted contract is stream-only
- **Payload field**: XADD field list
  - `op=auto_ban_candidate`
  - `key_id=<vendor key id>`
  - `consecutive_failures=<u32 as decimal string>`
  - `last_error_code=<string>`

### 3.4 Exact stream shape

Rust emits:

```text
XADD events:entity *
  op auto_ban_candidate
  key_id <vk_*>
  consecutive_failures <u32-decimal>
  last_error_code <GW_* or fallback code>
```

Field evidence:

- `op`: `crates/gw-store/src/redis_client.rs:44`
- `key_id`: `crates/gw-store/src/redis_client.rs:45`
- `consecutive_failures`: `crates/gw-store/src/redis_client.rs:46-49`
- `last_error_code`: `crates/gw-store/src/redis_client.rs:50`

### 3.5 Go-side requirement

- `events:entity` consumer must accept `op=auto_ban_candidate` as a new reverse-write op;
- recommended handling:
  1. parse `key_id`, `consecutive_failures`, `last_error_code`
  2. apply Go-side ban / review policy
  3. if a real state flip is desired, persist `vendor_keys` and publish the normal `ns=vendor_key id=<key_id> op=upsert|status_change` invalidation path.

---

## 4. `/healthz/active-requests` per-instance snapshot

**Status: Awaiting Go-side implementation**

### 4.1 Contract summary

- Rust now exposes `/healthz/active-requests` on the management-token-gated health surface;
- response is **per gateway instance**;
- Go may optionally scrape / aggregate multiple instances into a fleet-level active-request view.

### 4.2 Rust evidence

- Route exposure: `crates/gw-gateway/src/health.rs:137-143`
- Response builder: `crates/gw-gateway/src/health.rs:272-323`
- Format and model-group rollups: `crates/gw-gateway/src/health.rs:325-362`

### 4.3 Cross-process touchpoints

- **Redis stream key**: none
  - 证据: response is built from in-memory `InFlightStore.by_key` and cached entity metadata only; see `crates/gw-gateway/src/health.rs:290-315`
- **PgSQL column**: none directly
  - grouping metadata comes from resident `EntityStore` caches, not new PgSQL writes
- **Payload field / HTTP shape**: JSON body
  - `count: u64`
  - `by_format: object<string,u64>`
  - `by_model_group: object<string,u64>`

### 4.4 Exact JSON shape

```json
{
  "count": 7,
  "by_format": {
    "openai": 3,
    "anthropic": 2,
    "multi": 1,
    "unknown": 1
  },
  "by_model_group": {
    "mg_chat": 5,
    "unmapped": 2
  }
}
```

### 4.5 Go-side requirement

- Optional only;
- if Go wants cluster-wide observability, it should poll each gateway instance’s `/healthz/active-requests` and sum the three top-level buckets;
- no Rust write-back or acknowledgment path is required.

---

## 5. Scheduling presets → `model_groups.payload.strategy`

**Status: Rust implemented; awaiting Go-side producer update**

### 5.1 Contract summary

- Aether parity inventory tracks “preset dimension 集合 / scheduling presets” as a Split concern where **Go selects config, Rust executes routing**;
- Rust now supports `StrategyKind::MultiDim(String)` stored on `ModelGroup.strategy`;
- Go can persist a preset selection directly as `{"multi_dim":"<preset_name>"}` in `model_groups.payload.strategy`; Rust resolves `<preset_name>` against boot-time `[routing.presets]` and fails fast if it is missing.

### 5.2 Rust evidence

- Split background / preset inventory: `docs/ROADMAP.md`
- Shared enum contract: `crates/gw-core/src/entity.rs:233-244`
- `ModelGroup` payload field: `crates/gw-core/src/entity.rs:246-249`
- Weighted scoring: `crates/gw-routing/src/pipeline/multi_dim.rs`
- Runtime dispatch: `crates/gw-routing/src/strategy/set.rs`
- Boot-time preset config: `crates/gw-gateway/src/config.rs`, `config/default.toml`

### 5.3 Cross-process touchpoints

- **Redis stream key**: `events:entity` with `ns=model_group`
  - 证据路径: `docs/go-contracts/parts/01-go-as-producer.md:25-29`, `61-68`
  - Go updates `model_groups` then emits normal invalidation; no new stream name is introduced
- **PgSQL column**: `model_groups.payload`
  - 证据: `crates/gw-core/src/entity.rs:243-249` (`ModelGroup.strategy` deserializes from payload)
- **Payload field**: `strategy`
  - type: `StrategyKind`; legacy unit variants serialize with `serde(rename_all = "snake_case")`, while the preset variant uses Serde's externally-tagged enum object shape
  - multi-dim shape: `{"multi_dim":"balanced"}`
  - evidence: `crates/gw-core/src/entity.rs:233-244`

### 5.4 Accepted strategy set

Go may keep using existing strategy literals:

- `cache_affinity`
- `fixed_order`
- `round_robin`
- `load_balance`
- `cost_first`
- `latency_first`
- `weighted_rr`
- `sticky`

For scheduling presets, Go should write:

```json
"strategy": { "multi_dim": "balanced" }
```

The preset name must exist in the gateway's boot-time `[routing.presets]` table. Defaults are:

- `health_first`
- `latency_first`
- `cost_first`
- `balanced`

Evidence:

- enum values: `crates/gw-core/src/entity.rs:233-244`
- runtime dispatcher: `crates/gw-routing/src/strategy/set.rs`
- default presets: `config/default.toml` `[routing.presets]`

### 5.5 Go-side requirement

- Go may keep richer preset concepts internally, but any preset assigned to a model group must either be a legacy strategy literal or the `{"multi_dim":"<preset_name>"}` shape;
- custom preset names require the gateway config to define matching weights under `[routing.presets]` before startup; there is no hot reload or per-request override.

---

## 6. Compatibility notes

- Items 1 and 4 are HTTP-only split contracts; they intentionally add **no new Redis or PgSQL schema**.
- Items 2 and 3 are additive wire-contract expansions; old consumers remain valid if they ignore unknown fields / ops.
- Item 5 extends the already-landed Rust payload field (`model_groups.payload.strategy`) with the `{"multi_dim":"<preset_name>"}` variant while keeping existing strategy literals valid.
