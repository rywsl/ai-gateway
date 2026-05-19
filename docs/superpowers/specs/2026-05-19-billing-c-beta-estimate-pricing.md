# C-β — EstimatePricing entity + freeze 边界（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 6 B1 / B11 / B12）
  - [2026-05-19-billing-c-alpha-usage-record.md](./2026-05-19-billing-c-alpha-usage-record.md)（pricing_version 写入 UsageRecord）
  - [2026-05-19-billing-c-gamma-balance-split.md](./2026-05-19-billing-c-gamma-balance-split.md)（freeze 与三层余额读写分工）
  - [2026-05-18-llm-gateway-design.md](./2026-05-18-llm-gateway-design.md)（ADR-004 三层余额）
- **范围**：把 `gw-core::entity::Pricing` 拆为两层并存：
  - **`EstimatePricing`**：Rust 用，**保守估算价**——只保证"一个请求最多能花多少 micro"足够大，不保证准确；
  - **`BillingPricing`**：Go 用，完整多维 + 阶梯 + 公式（与 Aether 等价）；Rust 不读、不同步、不感知。
  Rust 网关只 load `EstimatePricing` 做 freeze；真实 cost 由 Go 端基于 `BillingPricing` 计算。
- **不在范围**：
  - `BillingPricing` 的字段表与 Go 端实现（→ C-ε 文档 + Go 自己 plan）；
  - UsageRecord 字段（→ C-α）；
  - 余额读写分工（→ C-γ）。

> 用户决议：Rust 只记录用量；公式归 Go。本 spec 把 Pricing 一分为二，让 freeze 保护机制保留，但 Rust 不背负任何"准确计费"职责。

---

## 1. 背景

当前 `Pricing` 结构（参 `gw-core::entity.rs:224`）：

```rust
pub struct Pricing {
    pub model: ModelName,
    pub input_price_micro_per_token: u64,
    pub output_price_micro_per_token: u64,
    pub max_output: u32,
    pub model_markup: f32,
    pub connect_timeout_ms: Option<u32>,
    pub read_timeout_ms: Option<u32>,
    pub total_timeout_ms: Option<u32>,
    pub stream_total_timeout_ms: Option<u32>,
}
```

被两个角色用：
1. `gw-proxy::filters::request.rs:404`（freeze estimate）—— 只需要"保守估算"；
2. `gw-proxy::filters::logging.rs:93`（settlement cost）—— 需要"真实公式"。

用户决议后角色 2 撤回（→ Go）；角色 1 保留，但**只用 estimate 字段**。多维计费 / 阶梯 / cache_creation 等都进 Go 的 `BillingPricing`，Rust 不感知。

`Pricing` 还混杂了 vendor timeout 字段——这些与计费无关，是路由/IO 行为，应该独立成 `VendorTimeouts` entity。本 spec 顺手拆开。

## 2. 设计决策

### ADR-Cβ-1 拆分 `Pricing`

```rust
// gw-core::entity::estimate_pricing

#[derive(Clone, Debug, Deserialize)]
pub struct EstimatePricing {
    pub model: ModelName,
    /// 保守估算的单 token 价（用于 freeze）。
    /// Go 端 push 时取所有 Billing 维度公式的**最大可能 per-token 输出**作为该字段：
    ///   max_per_token_micro = max(
    ///     output_per_token_micro,
    ///     cache_creation_per_token_micro,
    ///     thinking_per_token_micro,
    ///     ...
    ///   )
    /// 这样 freeze 一定 ≥ 真实 cost。
    pub conservative_per_token_micro: u64,
    /// `max_tokens` 缺省时的上限（Anthropic 必填，OpenAI 默认 4096 等）。
    pub default_max_tokens: u32,
    /// 多请求里允许的最大 token 数硬上限（hard cap 数据，与 ADR-024 同步）。
    pub hard_cap_tokens: u32,
    /// 安全因子（safety factor），freeze 估算时再乘一次（默认 1.0；危险模型可设 1.2）。
    pub safety_factor: f32,
}
```

撤销字段：`input_price_micro_per_token` / `output_price_micro_per_token` / `model_markup` / `max_output` / 所有 timeout 字段。

`model_markup` 完整移到 `MarkupChainSnapshot`（参 C-α）由 Go 端读 `BillingPricing` 算。

timeout 字段拆到独立 entity：

```rust
// gw-core::entity::vendor_timeouts

#[derive(Clone, Debug, Deserialize)]
pub struct VendorTimeouts {
    pub vendor_id: VendorId,
    pub model: Option<ModelName>,    // 可选：vendor 默认 vs 模型特化
    pub connect_timeout_ms: Option<u32>,
    pub read_timeout_ms: Option<u32>,
    pub total_timeout_ms: Option<u32>,
    pub stream_total_timeout_ms: Option<u32>,
}
```

`gw-store::loader::load_pricing` 拆成 `load_estimate_pricing` + `load_vendor_timeouts`。

### ADR-Cβ-2 freeze 计算公式

`gw-proxy::filters::request.rs` 当前的 `compute_freeze_estimate` 改为：

```rust
fn compute_freeze_estimate(
    estimate: &EstimatePricing,
    max_tokens: Option<u32>,
    input_tokens_est: u32,        // codec 提供的输入 token 数估算
) -> u64 {
    let output_cap = max_tokens.unwrap_or(estimate.default_max_tokens)
        .min(estimate.hard_cap_tokens);
    // 保守估算 = (输入 + 输出上限) × 单 token 保守价 × safety_factor
    let total_tokens = input_tokens_est as u64 + output_cap as u64;
    let raw = total_tokens * estimate.conservative_per_token_micro;
    ((raw as f64) * (estimate.safety_factor as f64)) as u64
}
```

注意：
- **不区分** input/output 价（Rust 不知道这些）；
- **不乘 markup**（markup 由 Go 在结算时乘；freeze 用最保守值）；
- **safety_factor 由 Go 设**（vendor 不稳定时调高）。

预期：freeze 估算值通常比真实 cost 高 20-50%，足以覆盖任何 markup chain。代价是冻结额度偏多——可接受，因为 freeze release 在请求结束时立即触发（不影响后续请求）。

### ADR-Cβ-3 entity 协议

`EstimatePricing` 走 ADR-013 Entity 协议：

```
Redis key:     entity:estimate_pricing:<model_name>
events stream: events:entity (namespace=estimate_pricing, op=upsert/delete)
PgSQL table:   estimate_pricings (Go 维护，Rust 不写)
```

Rust 通过 `gw-store::EntityStore` 懒加载（与现 Pricing 同款）。Go 是唯一 writer——push EstimatePricing 给 Rust 用，自己读 `BillingPricing` 算 cost。

`BillingPricing` entity：
- Redis key 例：`entity:billing_pricing:<model_name>`；
- 仅 Go 读写；Rust 完全不感知；
- 字段表 Go 自己定，可以参考 Aether `services/billing/templates.py`；
- 本 spec 不规范 `BillingPricing` 形态，避免锁住 Go 端实现自由度。

### ADR-Cβ-4 Pricing entity 迁移期

为不破坏现有部署，迁移分两步：

**Step 1（v1.x）**：保留旧 `Pricing` entity；新增 `EstimatePricing` entity；
- Rust 优先读 `EstimatePricing`，缺失时回落到旧 `Pricing`（取 `output_price_micro_per_token * 1.5` 作 conservative_per_token）；
- Go 同时维护两份；监控 fallback 比率。

**Step 2（v2.0）**：旧 `Pricing` 撤销；Go 完全切到 `BillingPricing`。

本 spec 描述 v2 目标态；step 1 兼容路径在实施 plan 写。

### ADR-Cβ-5 撤销 cost 计算

`gw-proxy::filters::logging.rs:93-105` 整段删除：

```rust
// 删除：
let cost_micro = if let Some(p) = ctx.pricing.as_ref() {
    let in_cost = ctx.usage.input_tokens as u64 * p.value.input_price_micro_per_token;
    let out_cost = ctx.usage.output_tokens as u64 * p.value.output_price_micro_per_token;
    let raw = in_cost + out_cost;
    let markup = ctx.merged_perm.as_ref()
        .map(|m| m.markup_product.max(1.0))
        .unwrap_or(1.0);
    ((raw as f32) * markup) as u64
} else {
    0
};
if let Some(g) = ctx.freeze_guard.as_ref() {
    g.settle(cost_micro);   // ← 这条语义改成 settle freeze 但不扣 deducted（参 C-γ）
}
```

替换为：

```rust
// freeze release：不扣 deducted，仅清 frozen counter
if let Some(g) = ctx.freeze_guard.as_ref() {
    g.release();   // ← 新方法名，明确不扣 deducted
}
```

`gw-ledger::FreezeGuard::settle(actual)` → `FreezeGuard::release()`，签名与语义都变。详见 C-γ。

### ADR-Cβ-6 vendor timeout 单独 entity

`gw-store::loader::load_vendor_timeouts(vendor_id)` 返回 `VendorTimeouts`。`gw-proxy::timeouts::Timeouts` 在装配阶段用它。这是顺手做的清理，**与 freeze 边界无关**，但避免 EstimatePricing 字段污染。

如果实施时发现 timeout 拆分过于侵入，可推后到独立 plan。本 spec 列为推荐但非阻塞。

### ADR-Cβ-7 性能预算

freeze estimate 计算：
- 当前 `compute_freeze_estimate` ~10 ns；
- 新版（少 2 个乘法 + 1 个 max）~5 ns；

EntityStore 读 EstimatePricing：
- 命中缓存 < 100 ns（Moka get）；
- miss 回 Redis 1-2 ms（与现 Pricing 同款）。

## 3. 实施大纲

依赖：C-α（UsageRecord 改造）；C-γ（freeze 接口签名）。

1. 新增 `gw-core::entity::EstimatePricing` + `VendorTimeouts`；
2. `gw-store::loader` 加 `load_estimate_pricing` / `load_vendor_timeouts`；保留 `load_pricing` 兼容路径；
3. `gw-store::EntityStore` 加 `estimate_pricing` Cache 字段；
4. `gw-proxy::filters::request::compute_freeze_estimate` 改用 `EstimatePricing`；
5. `gw-proxy::filters::logging` 删除 cost 计算（参 §ADR-Cβ-5）；
6. `gw-proxy::ctx::Ctx::pricing` 改类型从 `Option<Arc<Versioned<Pricing>>>` 到 `Option<Arc<Versioned<EstimatePricing>>>`；
7. `gw-proxy::timeouts::Timeouts` 改读 `VendorTimeouts`（若实施顺手做）；
8. 删除 `gw-core::entity::Pricing` 旧定义（或 deprecate 一个版本周期）；
9. 测试：
   - freeze 估算覆盖各模型 max_tokens；
   - safety_factor 调整影响估算值；
   - EntityStore 加载 / 失效 / 冷加载 round-trip；
   - 与 C-α 接口（pricing_version 字段写入 UsageRecord 正确）。

## 4. 未决问题

1. **conservative_per_token_micro 如何由 Go 计算**：Go 端读 BillingPricing 的所有维度，取最大值 + safety margin。具体公式 Go 自己决定，C-ε 文档化期望（必须 ≥ 真实可能的单 token 上限）。
2. **多模态价格折算**：Image / Audio 通常按"次数 + tokens"双计价，与 per-token 模型不直接可比。EstimatePricing 不区分维度，conservative_per_token 应该已经把多模态 worst-case 折成 per-token 价。是否真足够准？取决于业务侧 worst case 评估；倾向 v1 用 default_max_tokens × 文本上限做 estimate，多模态溢出由 safety_factor 兜底（设 1.5）。
3. **safety_factor 的运维位置**：放 EstimatePricing entity 字段还是全局 config？倾向 entity 字段（按模型独立调），但需要 Go 维护 UI；过渡期可以走全局 env `GW_FREEZE__SAFETY_FACTOR_DEFAULT`。
4. **`hard_cap_tokens` 与 ADR-024 的关系**：ADR-024（单请求 hard cap）当前由 `gw-auth::hard_cap` 独立维护。这里把它放 EstimatePricing 字段是冗余还是合并？倾向 EstimatePricing 持镜像（hard_cap 是 freeze 的 input），ADR-024 是权威源；Go 同时 push 到两个 entity。
5. **VendorTimeouts 是否必须本 spec 拆**：与 freeze 无关，但 Pricing 拆分时顺手做。若实施压力大可推后到独立小 plan，本 spec 范围内可降级为推荐。
6. **pricing_version 兼容性**：v1 fallback 路径下 EstimatePricing 缺失时 Rust 读旧 `Pricing` 计算 estimate——这种情况 pricing_version 用 0 还是 旧 Pricing 的 v？倾向用 0 + UsageRecord 加 flag `estimate_source: enum { Estimate, FallbackOldPricing }` 给 Go 监控。
7. **EstimatePricing 失败回路**：entity load 失败（Redis 不可达 / PgSQL 慢）时 Rust 该怎么 freeze？三选：(a) 短路 503；(b) 用全局 fallback 值（如 1000 micro/token）放行；(c) 拒绝 freeze 但允许请求过（事后由 Go 全额扣）。倾向 (a)：load 失败 = 系统级问题，应该拒服。但需要 metric `gw_freeze_estimate_pricing_unavailable_total`。

---

> C-β 完成后 Rust 完全不感知 cost 公式，但 freeze 保护未损。后续 BillingPricing 的形态由 Go 团队自己定，本 spec 不约束。
