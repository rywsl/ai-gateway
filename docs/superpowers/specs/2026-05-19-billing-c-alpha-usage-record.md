# C-α — UsageRecord schema 改造（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 6 B7 / B9 / B10）
  - [2026-05-19-codec-s4-response-and-cross-protocol.md](./2026-05-19-codec-s4-response-and-cross-protocol.md)（UsageSnapshot POD 来源）
  - [2026-05-19-billing-c-beta-estimate-pricing.md](./2026-05-19-billing-c-beta-estimate-pricing.md)（pricing_version 来源）
  - [2026-05-19-billing-c-epsilon-go-contract.md](./2026-05-19-billing-c-epsilon-go-contract.md)（Go 消费 UsageRecord stream）
- **范围**：把 `gw-ledger::BillingRecord` 改名为 `UsageRecord`，**删除 `cost_micro` 字段**；增加完整 `UsageSnapshot`（6 维 token 字段）、`pricing_version`、`markup_chain_snapshot`、其他给 Go 算 cost 时所需的快照字段。stream 名 `billing:records` 改为 `usage:records`。
- **不在范围**：
  - cost 算法本身（→ Go 侧 + C-ε 文档）；
  - EstimatePricing entity 形态（→ C-β）；
  - 余额三层读写分工（→ C-γ）；
  - DLQ 工程加固（→ C-δ）。

> 用户决议：**Rust 网关只做用量记录，cost 公式归 Go**。本 spec 是 Rust ↔ Go 之间 UsageRecord stream 的契约层。

---

## 1. 背景

当前 `gw-ledger::BillingRecord`（参 `crates/gw-ledger/src/billing.rs`）：

```rust
pub struct BillingRecord {
    pub ts_ms: u64,
    pub request_id: String,
    pub trace_id: String,
    pub user_id: UserId,
    pub child_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,
    pub client_protocol: String,
    pub upstream_protocol: String,
    pub model: ModelName,
    pub vendor_id: VendorId,
    pub vendor_key_id: KeyId,
    pub status: BillingStatus,
    pub error_code: Option<String>,
    pub http_status: u16,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
    pub cost_micro: u64,                  // ← 撤销
    pub freeze_amount_micro: u64,
    pub markup_product: f32,              // ← 改快照
    pub usage_source: UsageSource,
    pub retry_count: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub is_stream: bool,
}
```

问题：

- `cost_micro` 假设 Rust 自己算 cost——与新边界冲突；
- `input_tokens / output_tokens / cached_tokens` 只有 3 维，比桶 1 codec S4 已经规划的 `UsageSnapshot`（6 维）少；
- `markup_product` 只有合并后的标量乘积，丢失了链路（哪几层倍率连乘成的），Go 算时不可追溯；
- `freeze_amount_micro` 还在，但应配合 C-γ 改成"Rust frozen counter 已写入的值"快照。

## 2. 设计决策

### ADR-Cα-1 改名 + 字段重塑

`BillingRecord` → `UsageRecord`（语义化清晰：Rust 写的是"用量",不是"结算账单"）：

```rust
// gw-ledger::usage::UsageRecord（取代 BillingRecord）

#[derive(Debug, Serialize, Clone)]
pub struct UsageRecord {
    /// 唯一性
    pub ts_ms: u64,
    pub request_id: String,
    pub trace_id: String,

    /// 主体
    pub user_id: UserId,
    pub child_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,

    /// 协议路径
    pub client_protocol: String,
    pub upstream_protocol: String,
    pub model: ModelName,
    pub vendor_id: VendorId,
    pub vendor_key_id: KeyId,

    /// 结果
    pub status: UsageStatus,             // ← BillingStatus 改名 UsageStatus
    pub error_code: Option<String>,
    pub http_status: u16,

    /// 用量（核心字段，6 维 + 上游真实值标识）
    pub usage: UsageSnapshot,            // ← 取代旧 input_tokens / output_tokens / cached_tokens
    pub usage_source: UsageSource,       // Upstream / Estimated / FallbackConservative（沿用）

    /// 性能
    pub retry_count: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub is_stream: bool,

    /// 估算冻结快照（给 Go 对账 / 错误溯源用）
    pub freeze_amount_micro: u64,        // Rust 已 HINCRBY frozen +这个值
    pub estimate_pricing_version: u64,   // 用了哪版 EstimatePricing 算的 freeze

    /// Go 算 cost 时需要的快照（不是最终 cost）
    pub markup_chain_snapshot: MarkupChainSnapshot,
}

#[derive(Debug, Serialize, Clone)]
pub struct UsageSnapshot {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
    pub thinking_tokens: Option<u32>,
    pub multimodal_input_tokens: Option<u32>,
}

#[derive(Debug, Serialize, Clone)]
pub struct MarkupChainSnapshot {
    /// gw-auth merge_permissions 拼出来的链路快照——Go 用同源数据再算一次以校验。
    pub user_group_markup: f32,
    pub api_key_markup: f32,
    pub vendor_markup: f32,
    pub model_markup: f32,
    /// 兼容字段：合并后乘积（== 上面四个连乘），方便 Go 一眼对比。
    pub product: f32,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    Ok,
    Error,
    Cancelled,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Upstream,
    Estimated,
    FallbackConservative,
}
```

### ADR-Cα-2 删除 cost_micro

`cost_micro: u64` 删除——Rust 不算 cost。

如果 v1 临时兼容（避免 Go 端立刻断流），可以保留字段标 `#[deprecated]` + 一直写 0：

```rust
#[deprecated(note = "Rust no longer computes cost. Go computes from usage + pricing.")]
pub cost_micro: u64,  // always 0 in v2
```

倾向**直接删**（schema 改名同时改字段，一次性 PR；Go 团队同期改消费侧）。

### ADR-Cα-3 stream 名改 `usage:records`

Redis key 改名：

| 旧 | 新 |
|---|---|
| `billing:records` (XADD by Rust, consumed by Go for settlement) | `usage:records` (Rust XADD, Go 消费算 cost) |
| 无 | `settled_billing` (Go XADD, 内部审计 / 报表; Rust 不读) |

`gw-ledger::flusher::flush` 函数里硬编码的 stream name 改 `usage:records`。

向后兼容：v1 部署期间，Go 端 consumer group 同时订阅 `billing:records` 和 `usage:records`，确保过渡期不丢数据。一个版本周期后删 `billing:records`。

### ADR-Cα-4 usage_source 语义保留 + 扩展

`UsageSource::Upstream` 仍表示"上游真实回报"；`Estimated` 表示"流式中断/未拿到 final usage chunk → 用估算值"；`FallbackConservative` 表示"完全无 usage 信息 → 按 max_tokens 兜底"。

新增一种：
- `UsageSource::Partial`：流式拿到部分 chunk usage 但中断（Aether `usage/lifecycle.py` 区分过）。

`UsageSource` 影响 Go 算 cost 时是否打折（Estimated 通常按 100% 计；FallbackConservative 可能要 0.5x 折扣，留 Go 决定）。

### ADR-Cα-5 与 codec S4 的接口

桶 1 codec S4 ADR-S4-v2-5 已经规划 `extract_usage_any(wire) -> UsageSnapshot`。Rust 网关流程：

```
upstream response 到达
  ↓ gw-codec::extract_usage_any
UsageSnapshot (POD, copy)
  ↓ gw-proxy::filters::logging
UsageRecord (含 UsageSnapshot)
  ↓ Ledger::try_send
mpsc 通道
  ↓ flusher
XADD usage:records
```

UsageSnapshot 直接嵌入 UsageRecord 无额外转换。

### ADR-Cα-6 markup_chain_snapshot 来源

`gw-auth::merge_permissions` 当前算出 `markup_product`（单一 f32）。改造：

```rust
// gw-auth::merge::MergedPermissions
pub struct MergedPermissions {
    // ...原有字段
    pub markup_chain: MarkupChainSnapshot,  // ← 取代单一 markup_product
}
```

`gw-proxy::filters::logging` 直接把 `merged_perm.markup_chain` 拷进 UsageRecord。

四级倍率定义沿用主设计 ADR-023：user_group / api_key / vendor / model。每一层在 merge 阶段读对应 entity 的 markup 字段，相乘前保留。

### ADR-Cα-7 estimate_pricing_version

C-β 会规划 `EstimatePricing` entity 带 `version: u64` 字段（沿用 ADR-013 entity version）。Rust freeze 时用了哪版，把这个 version 一起写进 UsageRecord，Go 算 cost 时知道"Rust 当时基于这个版本的 estimate 做了冻结"——用于审计与失配检测。

### ADR-Cα-8 序列化格式

stream payload 仍走 JSON（`serde_json`）+ `XADD usage:records * payload <json>`，与现状一致。日后可替换为 msgpack（参 Aether 用 msgpack-base64）但 v1 不动。

### ADR-Cα-9 测试矩阵

- 多模态 / tools / thinking 响应 → UsageRecord 含完整 6 维；
- 流中断 → UsageSource=Partial；
- 完全无 usage → UsageSource=FallbackConservative；
- 跨协议响应（OpenAI 客户端 / Anthropic 上游）→ UsageRecord 的 client_protocol 与 upstream_protocol 字段正确；
- markup_chain_snapshot 四个字段连乘 == product。

## 3. 实施大纲

依赖：codec S4 land（UsageSnapshot 类型可用）。

1. 改 `crates/gw-ledger/src/billing.rs` → `usage.rs`，定义 `UsageRecord` + `UsageSnapshot`（若已在 gw-core 定义则复用）+ `UsageStatus` + `UsageSource` + `MarkupChainSnapshot`；
2. 撤销 `cost_micro` 字段；
3. 改 `gw-ledger::Ledger` mpsc 类型从 `BillingRecord` 改 `UsageRecord`；
4. 改 `gw-ledger::flusher::flush` 内部 stream 名 `billing:records` → `usage:records`；XADD 不再 INCRBY balance（这是 C-γ 范畴，本 spec 顺手清除即可）；
5. 改 `gw-auth::merge::MergedPermissions` 加 `markup_chain`；
6. 改 `gw-proxy::filters::logging`：
   - 删除 cost 计算（参 logging.rs:93-105）；
   - 用 codec S4 `extract_usage_any` 取 UsageSnapshot；
   - 装填 UsageRecord 各字段；
7. 删除 `gw-store::deduction::DeductionState::balance_micro` 的写入（这是 C-γ 范畴，本 spec 顺手清除）；
8. 测试 + benches 同步更新。

## 4. 未决问题

1. **UsageRecord 体积**：6 字段 UsageSnapshot + MarkupChainSnapshot (5 字段) + 其他元字段，序列化后 JSON ~600B。比旧 BillingRecord（~400B）多 50%。stream 内存与吞吐量影响：100 req/s = 60KB/s 写入，可接受。但 1000 req/s = 600KB/s。需要 Go 消费速率匹配，详见 C-δ。
2. **`FallbackConservative` 与 freeze 估算的关系**：当 UsageSource = FallbackConservative 时，Rust 已经按 max_tokens 估算冻结了一个偏高值；Go 算 cost 时如果用 0.5x 折扣会让用户少付，**Rust 冻结的 freeze_amount_micro 应该已经被减下来**——Rust 不知道，Go 也只能算实际 cost。这个保守冻结的"过冻"靠 freeze release 自然回。语义文档化即可。
3. **`pricing_version` 失配**：Rust 用 v=5 的 EstimatePricing 冻结，Go 拿到 UsageRecord 时它已经更到 v=7；Go 用 v=7 算 cost，Rust 冻结值与实际差距更大。可接受（Go 算的是当下权威值），但需要 metric `gw_billing_pricing_version_skew_total{rust_v, go_v}` 监控。
4. **stream 名改名的迁移期**：Go 团队需要同步切换 consumer。建议双写一个版本周期：v1.0 写 `billing:records`，v1.1 双写 `billing:records` + `usage:records`，v1.2 只写 `usage:records`。
5. **`UsageStatus::Cancelled` 是否要拆**：客户端取消 / 超时取消 / 行政取消 三种语义不同，Go 算 cost 时可能要不同处理。倾向 v1 不拆，error_code 字段载语义（如 `GW_REQUEST_CLIENT_CANCELLED`），Go 自己解码。
6. **markup_chain_snapshot 的 `product` 冗余字段**：四个 markup 连乘 == product，存 product 是冗余但便利。是否要靠 Go 自己算？倾向保留 product（< 4 字节），让 Go 端简化。

---

> C-α 是 Rust → Go 契约的核心。C-β 提供 freeze 估算输入；C-γ 接管余额读写分工；C-δ 加固 stream；C-ε 文档化 Go 期望。五份 spec 串成桶 6 v2 完整闭环。
