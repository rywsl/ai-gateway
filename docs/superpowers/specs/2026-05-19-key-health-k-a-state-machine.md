# K-A — KeyHealth 多维状态机（`gw-health` 业务 crate）（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界，未含实现）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 3 K3 + K4 + K7 + K8 入口）
  - [2026-05-19-business-split-refactor.md](../plans/2026-05-19-business-split-refactor.md)（业务/支撑分离原则）
  - [2026-05-19-key-health-k-b-vendor-error-hook.md](./2026-05-19-key-health-k-b-vendor-error-hook.md)（K-B 是 K-A 的"决策来源"——adapter 给 K-A 喂 CooldownDirective）
  - [2026-05-19-key-health-k-c-oauth-token.md](./2026-05-19-key-health-k-c-oauth-token.md)（K-C 也通过 K-A 向 routing 暴露不可用态）
- **范围**：新增业务 crate `gw-health`，承载 KeyHealth 多维状态机：
  - **多维分桶**：每个 (KeyId, ApiFormat) 独立的 sliding window + circuit state；
  - **HTTP 码分级冷却**：401 / 402 / 403 / 429 / 529 / 408/5xx 各自的 cooldown TTL；
  - **selection 接口**：`is_available(KeyId, ApiFormat) -> bool`，供 `gw-routing` 选 key 时调用；
  - **outcome 接口**：`record(KeyId, ApiFormat, OutcomeKind)`，供请求结束后 (proxy 或 adapter) 喂结果；
  - **cooldown 接口**：`apply_cooldown(KeyId, ApiFormat, CooldownDirective)`，供 K-B / K-C 显式标记冷却。
- **不在范围**：
  - vendor 特定的错误分类（→ K-B，adapter 决定 HTTP 码 / 响应体语义）；
  - OAuth token 缓存与刷新（→ K-C）；
  - quota probe（→ K-D）；
  - 状态持久化（v1 纯进程内，跨副本不共享——用户决议）。

> 撤回 `gw-routing::breaker`：当前 BreakerState 是"单维成功率"模型，K-A 完成后 breaker 整体迁移到 `gw-health`，多维 + 分级冷却替换。in_flight 计数器**保留**在 `gw-routing`（与 strategy::load_balance 强耦合），不进 gw-health。

---

## 1. 背景

`gw-routing::breaker.rs` 当前实现：滑动窗口 30s + 三态电路 + 单一阈值（≥20 样本，>50% 失败率，trip 60s）。无 HTTP 码区分、无 api_format 分桶、无显式冷却。

实际场景：

- 同一 vendor key 在 OpenAI 协议上稳定、在 Anthropic 协议上 429 — 单维 sliding window 把两个协议的样本混在一起，要么误 trip 要么漏 trip；
- 200 个 401（账号被删）和 200 个 503（上游瞬时） 等价 trip 是错的——前者应当永久冷却，后者只是短暂退避；
- 401 之后等 60s 重试是浪费配额（账号死了等 1h 都是白等），retry-after 之类的 hint 当前完全没看；
- 业务侧（K-B / K-C / K-D）需要一个**显式标记冷却**的入口，与 sliding window 推断出的 cooldown 共存。

Aether `services/health/monitor.py` + `pool/health_policy.py` 已经做了这些事。本 spec 把同款语义在 Rust 落地，但严格遵守业务/支撑分离（`gw-health` 是业务 crate）。

## 2. 关键判断

### ADR-KA-1 新建 `gw-health` 业务 crate

依赖方向：

```
gw-core    ◄── gw-health
gw-store   ◄── gw-health  （通用 entity / counter API，仅用于读 ApiKey / VendorKey 配置）
gw-health  ◄── gw-routing （routing 在选 key 时调 is_available；写 outcome）
gw-health  ◄── gw-proxy   （proxy 在请求结束记 outcome；adapter via K-B 喂 CooldownDirective 也最终走 proxy 或 adapter）
gw-health  ◄── gw-adapter （K-B / K-C 直接调）
```

**禁止**：
- `gw-health → gw-routing / gw-proxy / gw-adapter` —— 业务 crate 不依赖执行体；
- `gw-routing → gw-store::health_namespace` —— 任何 routing 改动不许在支撑层加 namespace（业务/支撑分离原则）。

### ADR-KA-2 数据结构

```rust
// gw-health::lib.rs

pub struct KeyHealthStore {
    /// 主索引：(KeyId, ApiFormat) → 该维度上的健康状态。
    by_key_format: DashMap<(KeyId, ApiFormat), Arc<FormatHealth>>,
}

pub struct FormatHealth {
    /// 滑动窗口：与现 gw-routing::breaker::SlidingWindow 同款（30s + 256 容量）。
    pub failure_window: Mutex<SlidingWindow>,
    /// 三态电路（沿用现状的 closed/open/half_open）。
    pub circuit: AtomicU8,
    /// open → half_open 的恢复 deadline（UNIX millis）。
    pub open_until_ms: AtomicU64,
    /// 显式冷却：cooldown_until_ms > now 时 is_available = false，
    /// 与 circuit 是 `OR` 关系（任一不通过都 unavailable）。
    pub cooldown_until_ms: AtomicU64,
    /// 冷却原因（最后一次写入），仅供观测/日志。
    pub cooldown_reason: AtomicU16,  // CooldownReason 编码为 u16
    /// 连续失败计数（用于 K-B 给出的 graded cooldown 翻倍策略）。
    pub consecutive_failures: AtomicU32,
}

#[repr(u16)]
pub enum CooldownReason {
    None = 0,
    Status401AccountDeactivated = 1,    // 长冷却 (1h)
    Status401Unauthorized = 2,           // 短冷却或不冷却（OAuth 刷新后立即恢复）
    Status402PaymentRequired = 3,        // 长冷却 (1h)
    Status403AccountSuspended = 4,       // 长冷却 (1h)
    Status403Forbidden = 5,              // 中冷却 (300s+)
    Status400OrganizationDisabled = 6,   // 长冷却 (1h)
    Status429RateLimit = 7,              // 按 retry-after 或默认
    Status529Overloaded = 8,             // 短冷却 (overload_cooldown_seconds)
    StatusTransient5xx = 9,              // 短冷却 (30s)
    Status408Timeout = 10,
    UnschedulableRule = 11,              // 关键词匹配（K-B 来源）
    OAuthRefreshFailed = 12,             // K-C 来源
    AccountBlocked = 13,                 // K-C 来源（账号级 block）
    AdminForce = 14,                     // 管理面手动冷却
    SlidingWindowTrip = 15,              // 由失败率自动 trip（非显式冷却）
}
```

`ApiFormat` 已存在或新加：

```rust
// gw-core::protocol（已有 ProtocolKind {OpenAi, Anthropic, Gemini}）
// 这里 ApiFormat 可以等同 ProtocolKind，或细化（如 openai_chat / openai_responses / openai_cli）
// v1 等同 ProtocolKind，避免分桶过细。
pub type ApiFormat = ProtocolKind;
```

### ADR-KA-3 selection 接口

```rust
impl KeyHealthStore {
    /// 选 key 时调用。检查电路状态 + 显式冷却。
    /// 闭路 + 无冷却 → true；半开时只有一个 caller 拿到 true（CAS 抢占 probe slot）。
    pub fn is_available(&self, key: &KeyId, fmt: ApiFormat) -> bool;

    /// 仅查询不抢占 probe slot——给观测/dashboard 用，不参与选 key。
    pub fn peek(&self, key: &KeyId, fmt: ApiFormat) -> HealthSnapshot;
}

pub struct HealthSnapshot {
    pub circuit: CircuitState,           // closed/open/half_open
    pub in_cooldown: bool,
    pub cooldown_until_ms: Option<u64>,
    pub cooldown_reason: CooldownReason,
    pub failure_rate_30s: f32,
    pub consecutive_failures: u32,
}
```

`is_available` 的语义（替代当前 `BreakerState::is_available`）：

1. 若 `cooldown_until_ms > now` → `false`；
2. 否则按现 circuit state 半开抢占 CAS 逻辑（沿用）；
3. **重要**：is_available 不应当有副作用（除了 half_open CAS），cooldown_until_ms 的解除由 outcome 路径处理。

### ADR-KA-4 outcome 记录接口

```rust
pub enum OutcomeKind {
    /// 请求 2xx
    Success,
    /// 请求失败，HTTP 码 + 是否带 retry-after（K-B 已解析）。
    /// 触发 sliding window 累计 + 自动判断是否需要 cooldown_until 提升。
    Failure { http_status: u16, retry_after_secs: Option<u32> },
}

impl KeyHealthStore {
    /// 由 gw-proxy::filters::logging 或 adapter 在请求结束时调用。
    /// 同时更新 sliding window、consecutive_failures、circuit state。
    /// 注意：这里不直接做"401 → cooldown 1h"的分级冷却决策——分级决策由 K-B 喂
    /// CooldownDirective 完成，本接口仅作累积统计与电路触发。
    pub fn record(&self, key: &KeyId, fmt: ApiFormat, outcome: OutcomeKind);
}
```

**分工边界**：
- `record(Failure { http_status: 503 })` —— gw-health 累积 sliding window，**不**主动设 cooldown；
- `apply_cooldown(reason=Status503Transient, ttl_secs=30)` —— 由 K-B 喂入（adapter 看了 503 自己决定走 transient 路径）；
- gw-health 只承载"通用电路 + 通用冷却字段"，**vendor/HTTP 码 → cooldown reason + ttl 的映射放 K-B**（adapter 各自实现）。

### ADR-KA-5 显式冷却接口

```rust
pub struct CooldownDirective {
    pub reason: CooldownReason,
    /// 冷却时长。
    pub ttl_secs: u32,
    /// 若 true：本次冷却**清除** consecutive_failures 计数（如 admin force / oauth refresh ok）；
    /// 若 false：累积，graded cooldown 时 K-B 可参考做 backoff。
    pub reset_consecutive: bool,
}

impl KeyHealthStore {
    /// 写入显式冷却。cooldown_until_ms = max(now + ttl_secs * 1000, cooldown_until_ms)
    /// （冷却窗口只增不减；新窗口必须晚于已有的——避免后续命令意外缩短）。
    pub fn apply_cooldown(&self, key: &KeyId, fmt: ApiFormat, directive: CooldownDirective);

    /// 显式清除冷却（OAuth 刷新成功后调；admin 手动 unblock 时调）。
    pub fn clear_cooldown(&self, key: &KeyId, fmt: ApiFormat);
}
```

### ADR-KA-6 自动恢复

cooldown_until_ms 到期 = 自动可用（is_available 路径上判断 now >= cooldown_until_ms 即 OK）。**不需要后台 reactivate 任务**（与 K7 K7「deactivation 自动恢复」要求一致）。

电路 open → half_open 的转换沿用现 breaker 的 CAS 半开抢占（参 `gw-routing::breaker.rs:135-162`）。这条逻辑迁移到 `FormatHealth::is_available` 内。

### ADR-KA-7 sliding window 与 cooldown 的优先级

| 当前 cooldown | sliding window 状态 | is_available 返回 |
|---|---|---|
| 未过期 | 任意 | false |
| 过期（或从未设过） | closed 且失败率 ≤ 阈值 | true |
| 过期 | open 且 open_until 未过 | false |
| 过期 | open 且 open_until 过 | CAS 抢半开 → true 给抢到的 caller |
| 过期 | half_open | false（其他 probe 在跑） |

cooldown 是显式信号，**优先级高于** sliding window 推断。

### ADR-KA-8 in_flight 计数器去向

`gw-routing::breaker::InFlightGuard` 与 strategy::load_balance + per-key cap 强耦合。**保留**在 `gw-routing`，不迁到 gw-health。理由：

- in_flight 是"选 key 时影响优先级"的支撑数据，与"key 是否健康"是不同语义；
- LoadBalance strategy 读 in_flight 是 `Ordering::Relaxed` 的 hot path，跨 crate 调用没必要。

迁移 detail：当前 `BreakerStore::by_key: DashMap<KeyId, Arc<BreakerState>>` 拆成：
- `gw-routing::in_flight::InFlightStore::by_key: DashMap<KeyId, Arc<InFlightCounter>>`（只剩 atomic u32）；
- `gw-health::KeyHealthStore`（多维健康）。

### ADR-KA-9 与 gw-routing::strategy 的接口

selection 时 strategy 调 `KeyHealthStore::is_available(key, ctx.api_format)` 过滤候选；不再调 `BreakerStore`。

`PickCtx` 增加 `api_format: ApiFormat` 字段（当前已有 `model`、`user_id`，加这一个）。

### ADR-KA-10 性能预算

| 操作 | 预算 |
|---|---|
| `is_available` 命中 closed + 无 cooldown | < 50 ns（两次 atomic load） |
| `is_available` 进入 half_open CAS 路径 | < 200 ns |
| `record(Success)` | < 100 ns（atomic + sliding window push） |
| `record(Failure)` 触发 sliding window 评估 | < 500 ns |
| `apply_cooldown` | < 100 ns（atomic store） |

新增 bench：`crates/gw-health/benches/state_machine.rs`。守住与现 breaker bench 同等级。

### ADR-KA-11 持久化

**v1：纯进程内**（用户决议）。

后果：

- 多副本网关各自维护一份 health；副本 A 看到某 key 401 已冷却 1h，副本 B 仍可以选它（直到 B 自己也撞一次 401）；
- 这是 trade-off：用户决议接受"每副本独立学一次"换"零 Redis RTT"；
- 后续 K-D quota probe 写回 entity 的设计与本 spec 解耦：probe 写 vendor_key 配置层（不是 health 层）。

### ADR-KA-12 可观测性

新增 metrics：

| Metric | Label | 说明 |
|---|---|---|
| `gw_health_circuit_state` | key, fmt, state | gauge：当前每 (key, fmt) 处于哪个 circuit |
| `gw_health_cooldown_active` | key, fmt, reason | gauge：当前有 cooldown 的 (key, fmt) + reason |
| `gw_health_record_total` | fmt, outcome | counter：record 调用次数 |
| `gw_health_apply_cooldown_total` | reason, source | counter：显式冷却次数 + 来源（K-B / K-C / admin） |
| `gw_health_unavailable_select_total` | key, fmt, cause | counter：选 key 时被 is_available 过滤的次数；cause 区分 circuit_open / cooldown |

## 3. 实施大纲（详细 step 进 plan）

1. 新建 `crates/gw-health` library crate（`Cargo.toml` + `src/lib.rs`）；
2. 实施 `SlidingWindow` + `FormatHealth` + `KeyHealthStore`（按 §ADR-KA-2 / 2-3 / 2-4 / 2-5）；
3. 把 `gw-routing::breaker::BreakerStore` 中 `in_flight` 部分剥到 `gw-routing::in_flight::InFlightStore`；其他逻辑作废；
4. `gw-routing::strategy::PickCtx` 加 `api_format` 字段；strategy 的 candidates 过滤改调 `KeyHealthStore::is_available`；
5. `gw-proxy::ctx` 把 `BreakerState` 引用换成 `KeyHealthStore` 引用；
6. `gw-proxy::filters::logging` 改调 `KeyHealthStore::record`（成功 → Success；失败 → Failure {status, retry_after}）；
7. `gw-routing::retry` 把"trip breaker" 改为"调 record(Failure)"——retry 不再独立通知 breaker；
8. 暴露 K-A 接口给 K-B / K-C / K-D 后续 spec 用；
9. 测试：
   - 9 对 (3 fmt × 3 scenario) 的 is_available 状态转移；
   - record 路径累积 + 自动 trip；
   - apply_cooldown 与 sliding window 的优先级；
   - half_open CAS 抢占在 5 个并发 caller 下只放行 1 个；
   - 性能 bench 守 §ADR-KA-10 预算。

## 4. 未决问题

1. **api_format 是否等同 ProtocolKind**：v1 假定等同（OpenAi/Anthropic/Gemini 三桶）。但 OpenAI 内部其实有 chat-completions、Responses、cli 三个子格式（Aether 用 `openai:chat / openai:cli / openai:compact`）。若细化分桶到 6 个，存储翻倍但精度上升。倾向 v1 用 ProtocolKind 三桶；细化推到后续。
2. **CooldownReason u16 编码的扩展性**：现在 16 个值，u16 还能扩展。是否需要让 K-B 自定义 reason？倾向不——所有 reason 集中在 `gw-health::CooldownReason` 枚举里维护（避免分散到 vendor adapter）；K-B 的 vendor-specific reason（如"organization_disabled"）只是 reason 的实例化，不新建 enum variant。
3. **half_open probe 失败后的退避**：当前 v1 半开失败一次就回 open 60s。这条 v1 沿用。若多次半开都失败，是否应延长 open_until？倾向加"open_attempts 累计 → open_until 倍增"，但本 spec 不实现，留 follow-up。
4. **跨 (KeyId, ApiFormat) 维度的全局保护**：某 key 在 OpenAI 失败 100 次，是否应当也降低它在 Anthropic 的可用性？v1 不做，严格隔离。若发现误判太多，再考虑"key 级 sliding window"作为补充信号。
5. **DashMap 内存增长**：每对 (KeyId, ApiFormat) 是个 entry。10000 key × 3 fmt = 30000 entry × ~200 bytes = ~6MB。可接受。但需要 dead entry 清理（key 被删后 entry 残留）——交给 EntityStore 的 invalidation 事件触发 `KeyHealthStore::evict(key)`。
6. **半开 probe 抢占是 (KeyId, ApiFormat) 维度的，跨副本不共享**：副本 A 拿到 probe，副本 B 同时也会拿到。这是 v1 纯进程内的代价。多副本场景下若 probe 误判风暴可能性低（概率叠加），但记录在 future risk register。
7. **`gw-health` 是否要 expose `gw-store` 的 Redis namespace**：v1 纯进程内 → 不需要 Redis。但 K-D quota probe 会写回 vendor_key entity（这是 gw-store 既有的 entity namespace，不是 health 的新 namespace）。本 spec 不动 gw-store。

---

> K-A 落地后，`gw-routing::breaker.rs` 部分作废；in_flight 留在 routing。后续 K-B / K-C / K-D 通过 `KeyHealthStore::apply_cooldown` 喂决策，互不依赖。
