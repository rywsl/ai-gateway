# R-AB — Routing Core 重写:Candidate Filter 链 + Multi-Score Dimension（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 4 S1/S2/S3/S4/S5/S8/S9）
  - [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md)（is_available + peek）
  - [2026-05-19-key-health-k-d-quota-probe.md](./2026-05-19-key-health-k-d-quota-probe.md)（VendorKey.quota_state）
  - [2026-05-19-billing-c-beta-estimate-pricing.md](./2026-05-19-billing-c-beta-estimate-pricing.md)（EstimatePricing 供 cost_first）
  - [2026-05-19-routing-r-e-failover.md](./2026-05-19-routing-r-e-failover.md)（消费 SmallVec 列表）
- **范围**：把当前 `gw-routing` 的两个核心模块整体重写：
  - **candidate filter**：从 `get_candidate_keys` 5 步硬编码线性过滤 → `CandidateFilter` trait 链；
  - **strategy selection**：从 `StrategyAny` enum 互斥单选 → `ScoringDimension` trait 加权 + `MultiScoreStrategy`；
  - **pick 签名**：`Option<&VendorKey>` → `SmallVec<[Arc<VendorKey>; 8]>` 排序列表；
  - **8 个 dimension 实现**：cache_affinity / cost_first / latency_first / load_balance / health_first / quota_balanced / plan_tier / recent_refresh；
  - **4 个 special strategy**：priority_first / round_robin / weighted_rr / sticky（**与 dimension 同级、不参与 multi-score**，绕过加权直接给 list）。
- **不在范围**：
  - LatencyTracker 实现（→ R-C，本 spec 仅消费其接口）；
  - affinity_manager（→ R-D，作为额外 dimension）；
  - failover 执行层（→ R-E）；
  - stream probe（→ R-F）；
  - Sticky 修复（→ R-G）。

> R-AB 是桶 4 的核心 spec，重写 routing 的两个核心抽象。本 spec land 后，routing 从"单维选 key"升级到"多维加权 + 完整排序列表"。

---

## 1. 背景

`gw-routing::candidates::get_candidate_keys`（参 `crates/gw-routing/src/candidates.rs`）当前是 30 行硬编码 5 步过滤：

```rust
if !matches!(key.status, KeyStatus::Active) { continue; }
if !key.accessible_models.contains(model) { continue; }
if !(key.accessible_endpoints.contains(&target_protocol) || key.protocol_conversion) { continue; }
if !perm.allowed_channels.is_empty() && !perm.allowed_channels.contains(&key.key_id) { continue; }
if breakers.in_flight(&key.key_id) >= breaker_in_flight_per_key { continue; }
```

问题：

- 加 filter 必须改函数（如接 K-A is_available、K-D quota_state、capability 等）；
- 每个 filter 没有 reject_reason 的可观测性；
- 测试只能"全过/全不过"二态，无法分别 mock 单个 filter。

`StrategyAny` enum 是 8 个互斥选项：业务只能选其一，不能"cache_affinity + quota_balanced + cost_first 加权"。Aether 的 multi-score 是把 14 个 dimension 当作可组合的打分项，加权求和后排序。

`Strategy::pick -> Option<&VendorKey>` 只返回首选，强迫 retry 自己决定要不要换 key——但跨 key fallback 是**调度**职责而非**重试**职责，逻辑应当解耦。

## 2. 设计决策

### ADR-RAB-1 CandidateFilter trait

```rust
// crates/gw-routing/src/filter.rs

pub trait CandidateFilter: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    /// 同步快速过滤（不能 await）。
    /// 返回 None = 通过；返回 Some(reason) = 拒绝 + 原因。
    fn admit(&self, key: &VendorKey, ctx: &CandidateCtx<'_>) -> Option<RejectReason>;
}

#[derive(Debug, Clone, Copy)]
pub struct RejectReason(pub &'static str);

pub struct CandidateCtx<'a> {
    pub user_id: &'a UserId,
    pub model: &'a ModelName,
    pub target_protocol: ProtocolKind,
    pub api_format: ApiFormat,
    pub merged_perm: &'a MergedPermissions,
    /// K-A 的健康存储（共享 Arc）
    pub health_store: &'a KeyHealthStore,
    /// in_flight 上限（每 key）
    pub per_key_inflight_cap: u32,
    /// in_flight 当前值的查询器
    pub in_flight_store: &'a InFlightStore,
}
```

预设 filter（按建议顺序 = 过滤率高→低 = 短路效率最好）：

| filter | 拒绝原因 | 备注 |
|---|---|---|
| `KeyStatusActive` | `"key_status_not_active"` | 配置层状态(Disabled/Expired/Depleted/RateLimited/Rotated/Deleted) |
| `ChannelAllowlist` | `"channel_not_in_allowlist"` | 五级权限合并结果 |
| `ModelAccess` | `"model_not_accessible"` | accessible_models 不含目标 |
| `ProtocolAccess` | `"protocol_not_accessible"` | accessible_endpoints 不含且 protocol_conversion=false |
| `KeyHealthAvailable` | `"key_unhealthy"` | K-A is_available(KeyId, ApiFormat) |
| `InFlightUnderCap` | `"in_flight_at_cap"` | per_key_inflight_cap |
| `CapabilityMatch` | `"capability_mismatch"` | 桶 10 CP1（thinking_required, cache_required 等），v1 留 hook |

filter 链构造：

```rust
pub struct FilterChain {
    filters: SmallVec<[Box<dyn CandidateFilter>; 8]>,
}

impl FilterChain {
    pub fn new() -> Self {
        Self { filters: SmallVec::new() }
    }
    pub fn with(mut self, f: Box<dyn CandidateFilter>) -> Self {
        self.filters.push(f);
        self
    }
    pub fn run(&self, key: &VendorKey, ctx: &CandidateCtx<'_>) -> Result<(), RejectReason> {
        for f in &self.filters {
            if let Some(reason) = f.admit(key, ctx) {
                metrics::counter!("gw_routing_candidate_rejected_total",
                    "filter" => f.name(),
                    "reason" => reason.0).increment(1);
                return Err(reason);
            }
        }
        Ok(())
    }
}
```

替换 `get_candidate_keys`：

```rust
pub async fn list_candidates(
    group: &ModelGroup,
    ctx: &CandidateCtx<'_>,
    store: &EntityStore,
    chain: &FilterChain,
) -> Vec<Arc<VendorKey>> {
    let mut out = Vec::with_capacity(group.vendor_key_ids.len());
    for kid in &group.vendor_key_ids {
        let Some(v) = store.vendor_key_or_load(kid).await else {
            continue;
        };
        if chain.run(&v.value, ctx).is_ok() {
            out.push(Arc::new(/* clone-with-secret, 与现 candidates.rs 同款 */));
        }
    }
    out
}
```

### ADR-RAB-2 ScoringDimension trait

```rust
// crates/gw-routing/src/dimension.rs

pub trait ScoringDimension: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    /// 计算单 key 在本维度上的偏好分数。
    /// 约定输出归一化到 [0.0, 1.0]，越高越偏好。
    /// 实现内部可访问 PickCtx 上挂的资源（latency_tracker / quota / health_peek 等）。
    fn score(&self, key: &VendorKey, ctx: &PickCtx<'_>) -> f32;

    /// 互斥组：同组多个 dimension 视为冲突（在 MultiScoreStrategy 构造时校验）。
    /// 例：cache_affinity 与 load_balance 是 "distribution" 互斥组。
    fn mutex_group(&self) -> Option<&'static str> { None }
}

pub struct PickCtx<'a> {
    pub user_id: &'a UserId,
    pub api_key_id: &'a ApiKeyId,
    pub model: &'a ModelName,
    pub api_format: ApiFormat,
    pub conversation_id: Option<&'a str>,
    pub message_hash: Option<u64>,
    pub health_store: &'a KeyHealthStore,
    pub in_flight_store: &'a InFlightStore,
    pub latency_tracker: &'a LatencyTracker,           // R-C
    pub affinity_manager: Option<&'a AffinityManager>, // R-D（可选）
    pub estimate_pricing: Option<&'a EstimatePricing>, // C-β（可选；按模型懒加载）
}
```

### ADR-RAB-3 MultiScoreStrategy 选取器

```rust
pub struct MultiScoreStrategy {
    pub dimensions: SmallVec<[(Arc<dyn ScoringDimension>, f32); 4]>,
}

impl MultiScoreStrategy {
    pub fn pick(&self, candidates: &[Arc<VendorKey>], ctx: &PickCtx<'_>)
        -> SmallVec<[Arc<VendorKey>; 8]>
    {
        if candidates.is_empty() {
            return SmallVec::new();
        }
        // 1. 算每 key 的总分
        let mut scored: SmallVec<[(f32, &Arc<VendorKey>); 16]> = SmallVec::new();
        for key in candidates {
            let mut score = 0.0;
            for (dim, weight) in &self.dimensions {
                score += dim.score(key, ctx) * weight;
            }
            scored.push((score, key));
        }
        // 2. 按分数降序排序
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        // 3. 输出排序列表
        scored.into_iter().map(|(_, k)| Arc::clone(k)).collect()
    }
}
```

构造时校验互斥组：

```rust
impl MultiScoreStrategy {
    pub fn build(dims: Vec<(Arc<dyn ScoringDimension>, f32)>) -> Result<Self, RoutingConfigError> {
        let mut groups = HashMap::new();
        for (d, _) in &dims {
            if let Some(g) = d.mutex_group() {
                if let Some(other) = groups.insert(g, d.name()) {
                    return Err(RoutingConfigError::MutexGroupConflict {
                        group: g, a: other, b: d.name(),
                    });
                }
            }
        }
        Ok(Self { dimensions: SmallVec::from_vec(dims) })
    }
}
```

### ADR-RAB-4 SelectorAny enum

不是所有 strategy 都走 multi-score。**special strategy**(round_robin / weighted_rr / sticky / priority_first / fixed_order) 有特定语义不适合加权，单独保留。`SelectorAny` 替换原 `StrategyAny`：

```rust
pub enum SelectorAny {
    /// 多维加权
    MultiScore(MultiScoreStrategy),
    /// 严格按 priority desc，平级随机
    PriorityFirst,
    /// 简单轮询
    RoundRobin(RoundRobin),
    /// 加权轮询
    WeightedRr(WeightedRr),
    /// 会话粘性（参 R-G）
    Sticky(Sticky),
    /// 完全固定顺序（priority desc + key_id tiebreak，确定性）
    FixedOrder,
}

impl SelectorAny {
    pub fn pick(&self, candidates: &[Arc<VendorKey>], ctx: &PickCtx<'_>)
        -> SmallVec<[Arc<VendorKey>; 8]>
    {
        match self {
            SelectorAny::MultiScore(m) => m.pick(candidates, ctx),
            SelectorAny::PriorityFirst => sort_by_priority_desc(candidates),
            SelectorAny::RoundRobin(r) => r.pick_ordered(candidates),
            SelectorAny::WeightedRr(w) => w.pick_ordered(candidates, ctx),
            SelectorAny::Sticky(s) => s.pick_ordered(candidates, ctx),
            SelectorAny::FixedOrder => sort_by_priority_then_id(candidates),
        }
    }
}
```

**所有 variant 都返回 SmallVec 排序列表**，与 R-E failover 接口对齐。

### ADR-RAB-5 8 个 dimension 实现

#### CacheAffinity

```rust
pub struct CacheAffinityDimension;

impl ScoringDimension for CacheAffinityDimension {
    fn name(&self) -> &'static str { "cache_affinity" }
    fn mutex_group(&self) -> Option<&'static str> { Some("distribution") }

    fn score(&self, key: &VendorKey, ctx: &PickCtx<'_>) -> f32 {
        let Some(msg_hash) = ctx.message_hash else { return 0.5; };  // 无 hash 时中性
        // 把所有 candidate 在 stable 顺序下编号：先按 priority desc + key_id tiebreak
        // hash 映射到 idx，对该 idx 返回 1.0，其他返回 0.0
        // 这里需要 candidate 完整 list；本 dim 单独看一个 key 给不出"是否被选中"
        //
        // 实现策略：dim 在 score 时不能看全 list，但 MultiScoreStrategy 给 dim 一个
        // `target_index` 参数（通过 dim 注册时声明 needs_global_view） →
        // multi_score 先做一次 pre-pass 选 cache target，再调 dim.score。
        //
        // v1 简化：dim 内部用 key_id.hash 与 msg_hash 做距离评分（汉明距离归一）。
        let key_hash = stable_hash(&key.key_id);
        let dist = (key_hash ^ msg_hash).count_ones() as f32 / 64.0;
        1.0 - dist  // 距离越近分越高
    }
}
```

**注**：cache_affinity 在 multi-score 里的精确语义比 v1 复杂——真正"映射到一个固定 idx"需要 dim 看到全 list。v1 用汉明距离近似，足够把同 hash 的请求集中到同一 key。精确实现留 follow-up。

#### CostFirst

```rust
pub struct CostFirstDimension;

impl ScoringDimension for CostFirstDimension {
    fn name(&self) -> &'static str { "cost_first" }

    fn score(&self, key: &VendorKey, ctx: &PickCtx<'_>) -> f32 {
        // EstimatePricing 提供 conservative_per_token_micro；越低越偏好（cost first）
        let Some(p) = ctx.estimate_pricing else { return 0.5; };
        let per_token = p.conservative_per_token_micro;
        // 假设业务常见 per_token 在 1..10000 micro 之间；做对数归一
        let log_norm = (per_token as f32).max(1.0).ln() / 10.0;
        (1.0 - log_norm).clamp(0.0, 1.0)
    }
}
```

#### LatencyFirst

```rust
pub struct LatencyFirstDimension;

impl ScoringDimension for LatencyFirstDimension {
    fn name(&self) -> &'static str { "latency_first" }

    fn score(&self, key: &VendorKey, ctx: &PickCtx<'_>) -> f32 {
        // R-C LatencyTracker 提供 P50 TTFT（ms）；越低越偏好
        let p50 = ctx.latency_tracker.p50_ttft_ms(&key.key_id, ctx.api_format)
            .unwrap_or(500);  // 默认 500ms（中位 placeholder）
        // 0..5000ms 范围归一化
        let norm = (p50 as f32 / 5000.0).clamp(0.0, 1.0);
        1.0 - norm
    }
}
```

#### LoadBalance

```rust
pub struct LoadBalanceDimension {
    pub alpha_inflight: f32,
    pub beta_latency: f32,
    pub gamma_error_rate: f32,
}

impl ScoringDimension for LoadBalanceDimension {
    fn name(&self) -> &'static str { "load_balance" }
    fn mutex_group(&self) -> Option<&'static str> { Some("distribution") }

    fn score(&self, key: &VendorKey, ctx: &PickCtx<'_>) -> f32 {
        let in_flight = ctx.in_flight_store.in_flight(&key.key_id) as f32;
        let latency = ctx.latency_tracker.p50_ttft_ms(&key.key_id, ctx.api_format)
            .unwrap_or(500) as f32;
        let snap = ctx.health_store.peek(&key.key_id, ctx.api_format);
        let error_rate = snap.failure_rate_30s;

        // 三项各自归一化 + 加权 + 取负（lower-better → score = 1 - normalized）
        let in_flight_norm = (in_flight / 100.0).clamp(0.0, 1.0);
        let latency_norm = (latency / 5000.0).clamp(0.0, 1.0);
        let composite = self.alpha_inflight * in_flight_norm
                      + self.beta_latency * latency_norm
                      + self.gamma_error_rate * error_rate;
        1.0 - composite.clamp(0.0, 1.0)
    }
}
```

默认 α=1, β=0.5, γ=2.0（error_rate 权重最大）。

#### HealthFirst

```rust
pub struct HealthFirstDimension;

impl ScoringDimension for HealthFirstDimension {
    fn name(&self) -> &'static str { "health_first" }

    fn score(&self, key: &VendorKey, ctx: &PickCtx<'_>) -> f32 {
        let snap = ctx.health_store.peek(&key.key_id, ctx.api_format);
        // 拒绝 cooldown 或 circuit open 的 key 已由 filter 处理；这里只在 admitted 之间区分
        let failure_rate = snap.failure_rate_30s;
        let consecutive = snap.consecutive_failures as f32;
        let consecutive_norm = (consecutive / 10.0).clamp(0.0, 1.0);
        1.0 - failure_rate * 0.7 - consecutive_norm * 0.3
    }
}
```

#### QuotaBalanced

```rust
pub struct QuotaBalancedDimension;

impl ScoringDimension for QuotaBalancedDimension {
    fn name(&self) -> &'static str { "quota_balanced" }

    fn score(&self, key: &VendorKey, _ctx: &PickCtx<'_>) -> f32 {
        let Some(remaining) = key.quota_state.remaining else { return 0.5; };  // 未知 quota 中性
        // 1000+ tokens 充足；< 100 紧张
        let log_norm = (remaining as f32).max(1.0).ln() / 15.0;
        log_norm.clamp(0.0, 1.0)
    }
}
```

#### PlanTier

```rust
pub struct PlanTierDimension {
    /// 偏好顺序：tier 名 → 分数（高分 = 优先）。
    /// 如 {"free": 1.0, "plus": 0.7, "team": 0.5, "family": 0.3}
    pub tier_scores: HashMap<&'static str, f32>,
}

impl ScoringDimension for PlanTierDimension {
    fn name(&self) -> &'static str { "plan_tier" }

    fn score(&self, key: &VendorKey, _ctx: &PickCtx<'_>) -> f32 {
        // VendorKey 加 plan_type 字段（Go 端写入；K-D quota probe 可推算）
        let plan = key.plan_type.as_deref().unwrap_or("unknown");
        self.tier_scores.get(plan).copied().unwrap_or(0.5)
    }
}
```

VendorKey 加 `plan_type: Option<String>` 字段——这是本 spec 的 schema 改动需求。

#### RecentRefresh

```rust
pub struct RecentRefreshDimension {
    /// 多久之内 refresh 算"新鲜"（毫秒）
    pub window_ms: u64,
}

impl ScoringDimension for RecentRefreshDimension {
    fn name(&self) -> &'static str { "recent_refresh" }

    fn score(&self, key: &VendorKey, ctx: &PickCtx<'_>) -> f32 {
        // OauthCache 持有 last_refresh_attempt_ms；需要在 PickCtx 暴露 oauth_cache，或
        // 通过 health_store 暴露一个 sentinel（已 refresh 的 key 自动 clear_cooldown
        // 时间作为指纹）。
        //
        // v1 简化：用 VendorKey.expires_at（OAuth token 过期时间）做近似——
        // 离过期越远 = 越新鲜。
        let Some(exp) = key.expires_at else { return 0.5; };
        let now = OffsetDateTime::now_utc();
        let remaining = (exp - now).whole_milliseconds() as i64;
        let norm = (remaining as f32 / self.window_ms as f32).clamp(0.0, 1.0);
        norm
    }
}
```

#### 8 个 dimension 的互斥组与默认权重

| dimension | mutex_group | 默认 weight |
|---|---|---|
| cache_affinity | distribution | 0.0（默认不启用） |
| cost_first | - | 0.3 |
| latency_first | - | 0.2 |
| load_balance | distribution | 1.0 |
| health_first | - | 1.5 |
| quota_balanced | - | 0.8 |
| plan_tier | - | 0.5 |
| recent_refresh | - | 0.1 |

业务通过 ModelGroup entity 上的 `routing_config` 字段配置启用哪几个 + 权重。

### ADR-RAB-6 ModelGroup 配置 schema

```rust
// gw-core::entity::ModelGroup
pub struct ModelGroup {
    // ...原字段
    pub routing_config: RoutingConfig,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RoutingConfig {
    /// 选择器类型
    pub selector: SelectorKind,
    /// multi-score 时启用的 dimension（dim 名 → 权重）
    pub multi_score: Option<HashMap<String, f32>>,
}

pub enum SelectorKind {
    MultiScore,
    PriorityFirst,
    RoundRobin,
    WeightedRr,
    Sticky,
    FixedOrder,
}
```

Go 写入，Rust 读取 → 在 `bootstrap` 阶段建 SelectorAny。

### ADR-RAB-7 性能预算

| 操作 | 预算 |
|---|---|
| `FilterChain::run`(8 filter + key) | < 100 ns（atomic load + HashMap get） |
| `list_candidates`(100 key) | < 30 µs（含 entity store 命中缓存路径） |
| `MultiScoreStrategy::pick`(20 candidates × 5 dimension) | < 5 µs（线性算分 + sort_by） |
| `SelectorAny::PriorityFirst` | < 500 ns（sort_by_key） |
| `SelectorAny::RoundRobin` | < 200 ns（atomic counter） |

新增 benches：
- `gw-routing/benches/filter_chain.rs`；
- `gw-routing/benches/multi_score.rs`（5 dim × 20 candidate）。

### ADR-RAB-8 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_routing_candidate_admitted_total` | model, fmt | counter |
| `gw_routing_candidate_rejected_total` | filter, reason | counter |
| `gw_routing_select_total` | selector | counter |
| `gw_routing_multi_score_dimensions_active` | dim_name | gauge：当前 ModelGroup 启用的 dim |
| `gw_routing_pick_duration_us` | selector | histogram |
| `gw_routing_pick_list_size` | selector | histogram：输出列表长度 |

## 3. 实施大纲

依赖：K-A land（KeyHealthStore + InFlightStore） + R-C land（LatencyTracker stub 至少要 trait 存在）+ C-β land（EstimatePricing）。

1. 撤销 `gw-routing::candidates::get_candidate_keys`；
2. 新建 `gw-routing::filter` 模块（trait + 7 个预设 filter + FilterChain）；
3. 新建 `gw-routing::dimension` 模块（trait + 8 个 dimension）；
4. 新建 `gw-routing::multi_score` 模块（MultiScoreStrategy）；
5. 重写 `gw-routing::strategy::mod` → `selector::mod`，定义 `SelectorAny` enum；
6. 删除旧 8 个 strategy 文件（cost_first / latency_first / load_balance / cache_affinity 整体重写为 dimension）；保留 round_robin / weighted_rr / fixed_order / sticky 作为 special selector（sticky 由 R-G 修复）；
7. `gw-core::entity::ModelGroup` 加 routing_config 字段；
8. `gw-core::entity::VendorKey` 加 plan_type 字段（K-D 已规划元数据；这里转 const 字段）；
9. `gw-proxy::filters::upstream` 改用新 list_candidates + Selector::pick → 拿到 SmallVec 后传给 R-E 执行层（本 spec 把"用 list 的第一个"作为占位，R-E 真正实现 fallback）；
10. 测试矩阵：
    - 每个 filter：admit/reject 决策；
    - filter chain：拒绝原因聚合；
    - 每个 dimension：score 边界值（0/1/中性）；
    - mutex group 冲突检测；
    - MultiScoreStrategy：8 dim × 20 key 排序正确；
    - SelectorAny 各 variant 返回 SmallVec 一致。

## 4. 未决问题

1. **cache_affinity dim 的 global_view 问题**: v1 用汉明距离近似。精确实现需要 dim 在 pick 阶段看到完整 candidate list（"哪个 idx 中目标"）；这破坏 ScoringDimension 单 key 接口。两种解法：(a) ScoringDimension 加 `score_batch(&[&VendorKey], ctx) -> Vec<f32>` 默认 fallback 到 score；(b) cache_affinity 特化为 MultiScoreStrategy 的 pre-pass（先选 cache target，对该 target 加分）。倾向 (a)，trait 可扩展；v1 仅实现 score，cache_affinity 用近似。
2. **PlanTierDimension 的 tier 集合**: free/plus/team/family/enterprise/...，可能上百种。是否要做"未知 tier 降级到中性 0.5"？倾向是；未知 tier 不应短路调度。
3. **dim 权重的归一化**: 用户配的权重和不一定为 1（如 health=1.5 + cost=0.3 + quota=0.8 = 2.6），score 加权后可能超 1。是否归一化？倾向**不**——dim 内部已归一到 [0,1]，加权和虽不严格在 [0,1] 但同请求内排序意义不变。
4. **dim 的 cold cache**: cost_first 需要 EstimatePricing；首次访问该模型时 entity miss → load Redis/PgSQL。dim::score 不能 await（trait 是同步的）。倾向 list_candidates 之前由调用方预 await 装填到 PickCtx；若 miss → cost_first dim 给 0.5 中性分。
5. **RecentRefreshDimension 数据源**: 倾向 v2 让 OauthCache 暴露 last_refresh_ms 接口；v1 用 expires_at 近似（同向变化）。
6. **SelectorAny::WeightedRr 输出 SmallVec 排序**: WRR 本质是"按权重抽签选一个"，输出排序列表的语义是什么？倾向：weighted 抽一个为第一，其余按 priority desc + key_id tiebreak 排作为 fallback。
7. **`list_candidates` 是否要 await**: entity_store 走 Moka 缓存命中时同步；miss 时 await Redis/PgSQL。倾向 list_candidates async，热路径打到缓存命中 < 30 µs，miss 在 cold path（首次访问该 ModelGroup 后续命中）。
8. **plan_type 字段 entity 迁移**: VendorKey 加字段是 schema change，需要 Go 管理面 UI + 老数据兼容。倾向 default = `None`，旧 key 不指定 plan_type 时 PlanTierDimension 给 0.5。

---

> R-AB 是桶 4 核心，落地后 routing 真正具备多维选择能力。R-C/R-D 补 dimension 数据来源，R-E 消费排序列表，R-F 加 stream 安全网，R-G 修 Sticky。
