# K-D — Quota Probe + Upstream Usage Snapshot（v2026-05-19）

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界，未含实现）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 3 K5 + K6）
  - [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md)（K-D 用 K-A 的 is_available 跳过已冷却 key）
  - [2026-05-19-key-health-k-c-oauth-token.md](./2026-05-19-key-health-k-c-oauth-token.md)（K-D 通过 K-C 拿 access_token）
  - [2026-05-19-codec-s4-response-and-cross-protocol.md](./2026-05-19-codec-s4-response-and-cross-protocol.md)（UsageSnapshot 形态复用）
- **范围**：
  - 独立后台 scheduler（与请求路径完全解耦），按 vendor 协议定期 probe 上游剩余配额；
  - 解析每次业务请求响应里的 usage 头/字段，回写到 vendor_key entity 上的"剩余配额"字段；
  - routing 通过现有 `cost_first` / multi-score 策略读"剩余配额"做选择优化（**本 spec 不动 routing 策略**——只暴露字段）。
- **不在范围**：
  - 具体 vendor probe endpoint 与协议（→ vendor adapter plan，如 codex_quota）；
  - routing 调度策略（→ 桶 4 / multi-score spec）；
  - billing 用量回滚（→ billing spec）。

> K-D 是桶 3 唯一**主动行为**的子项——其他 K-A/K-B/K-C 都是被动响应。K-D 解决"账号配额耗尽时还选它 → 必败"的情境。

---

## 1. 背景

某些上游有"账号配额"概念，例如：

- Codex（OpenAI 内部端点）有 `x-codex-quota-remaining` 头；
- Anthropic 通过 `anthropic-ratelimit-*-remaining` 系列头报告余量；
- Gemini CLI / Vertex 有 quota project 级别配额；
- 某些 OAuth vendor 的 dashboard API 暴露剩余调用次数。

如果网关不知道这些信息，会出现：

- 选 key 时不知道某 key 已经接近上限 → 再发请求就 429；
- 多个 key 优先级相同，但其中一个 quota 充足、另一个快耗尽 → 应优先用充足的；
- 重置周期到达时（每天/每月）无法主动恢复——只能等下一次失败 retry。

Aether 用 `provider_keys/pool_quota_probe_scheduler.py` + `codex_realtime_quota.py` + `usage/upstream_usage_snapshot.py` 实现两条互补的路径：

1. **被动 snapshot**：业务请求响应里带 quota 字段 → 解析 → 写回；
2. **主动 probe**：后台定期发空请求探测 → 解析 → 写回。

本 spec 把两路在 Rust 落地。

## 2. 关键判断

### ADR-KD-1 模块归属

- **被动 snapshot 解析器** = vendor-specific 数据提取，归 `gw-adapter`；
- **主动 probe scheduler** = 独立后台任务，归 `gw-adapter::quota_probe`（仍在 adapter 业务 crate，与 OAuth refresher 同层）；
- **存储字段** = `entity:vendor_key:<id>` 上扩列（gw-store 已有的通用 entity 协议，不新建 namespace）。

依赖方向：

```
gw-core    ◄── gw-adapter::quota
gw-store   ◄── gw-adapter::quota   （写 entity:vendor_key.quota_*）
gw-health  ◄── gw-adapter::quota   （probe 前用 is_available 跳过冷却 key）
gw-adapter::oauth ◄── gw-adapter::quota  （probe 通过 K-C 拿 access_token）
```

### ADR-KD-2 entity 字段扩列

在 `entity:vendor_key:<key_id>` 上加 OAuth 字段（K-C）的同一份 HASH 上再加：

```
quota_remaining_tokens         u64      # 余量 token 数（vendor 报多少就存多少）
quota_remaining_unit           u8       # 0=tokens / 1=requests / 2=usd_micro / 255=unknown
quota_remaining_window_secs    u32      # 距下次 reset 多少秒（vendor 报告）
quota_last_snapshot_ms         u64      # 上次更新（被动 + 主动合并）
quota_probe_last_attempt_ms    u64      # 上次主动 probe 时间
quota_probe_consecutive_fails  u16      # 连续 probe 失败次数（用于退避）
quota_last_source              u8       # 0=snapshot(响应回写) / 1=probe(主动)
v                              u64      # ADR-013 版本号
```

`gw-store::loader` 解析 `VendorKey` 时把这些字段读到 `VendorKey::quota_state: QuotaState` 子结构上（新加；不破坏 K-A / K-B 字段）。

```rust
// gw-core::entity::VendorKey 扩字段
pub struct VendorKey {
    // ...
    pub quota_state: QuotaState,
}

#[derive(Default, Clone, Debug)]
pub struct QuotaState {
    pub remaining: Option<u64>,
    pub unit: QuotaUnit,
    pub window_secs: Option<u32>,
    pub last_snapshot_ms: u64,
    pub last_source: QuotaSource,
}

pub enum QuotaUnit { Tokens, Requests, UsdMicro, Unknown }
pub enum QuotaSource { Snapshot, Probe, None }
```

### ADR-KD-3 被动 snapshot 解析器

```rust
// gw-adapter::quota::snapshot

pub trait QuotaSnapshotExtractor: Send + Sync + 'static {
    fn vendor_id(&self) -> &VendorId;

    /// 从响应头 + body 提取配额信息。
    /// 返回 None 表示该响应未带 quota 信息。
    fn extract(&self, headers: &[(String, String)], body_preview: &[u8])
        -> Option<QuotaExtract>;
}

pub struct QuotaExtract {
    pub remaining: u64,
    pub unit: QuotaUnit,
    pub window_secs: Option<u32>,
}
```

每个 OAuth vendor 提供实现：

```rust
// gw-adapter::codex::quota
pub struct CodexQuotaExtractor;
impl QuotaSnapshotExtractor for CodexQuotaExtractor {
    fn extract(&self, headers: &[(String, String)], _body: &[u8]) -> Option<QuotaExtract> {
        let remaining = parse_header_u64(headers, "x-codex-quota-remaining")?;
        let window = parse_header_u32(headers, "x-codex-quota-reset-after").ok();
        Some(QuotaExtract {
            remaining,
            unit: QuotaUnit::Tokens,
            window_secs: window,
        })
    }
}

// gw-adapter::anthropic::quota
pub struct AnthropicQuotaExtractor;
impl QuotaSnapshotExtractor for AnthropicQuotaExtractor {
    fn extract(&self, headers: &[(String, String)], _body: &[u8]) -> Option<QuotaExtract> {
        let remaining_tokens = parse_header_u64(headers, "anthropic-ratelimit-tokens-remaining")?;
        // ... 还有 requests-remaining，看哪个更紧
        Some(QuotaExtract {
            remaining: remaining_tokens,
            unit: QuotaUnit::Tokens,
            window_secs: parse_header_u32(headers, "anthropic-ratelimit-tokens-reset").ok(),
        })
    }
}
```

### ADR-KD-4 snapshot 集成点

在 `gw-proxy::filters::response` 或 logging 阶段调用：

```rust
async fn on_upstream_response(ctx: &mut ReqCtx, headers: &[...], body_preview: &[u8]) {
    if let Some(extractor) = adapter_registry.quota_extractor_for(&ctx.vendor_id) {
        if let Some(extract) = extractor.extract(headers, body_preview) {
            quota_writer.write(&ctx.vendor_key_id, extract, QuotaSource::Snapshot).await;
        }
    }
}
```

`quota_writer.write` 内部走 `gw-store::entity` 协议，HSET 那几个字段 + ZADD 到 `events:entity`（让其他副本看到更新）。写入有 throttle：同一 key 30 秒内只允许写一次（避免 high-RPM 场景下 1000 次/s 的 HSET 风暴）。

### ADR-KD-5 主动 probe scheduler

```rust
// gw-adapter::quota::probe

pub struct QuotaProbeScheduler {
    health_store: Arc<KeyHealthStore>,
    oauth_cache: Arc<OauthCache>,
    quota_writer: Arc<QuotaWriter>,
    probers: HashMap<VendorId, Arc<dyn QuotaProber>>,
    /// 每 vendor 的 probe 频率（默认 10 分钟）
    interval_by_vendor: HashMap<VendorId, Duration>,
}

#[async_trait]
pub trait QuotaProber: Send + Sync + 'static {
    /// 调上游 probe endpoint（例如 GET /usage 或 HEAD /v1/messages）。
    /// 返回结构与 snapshot 一致。
    async fn probe(&self, key: &VendorKey, access_token: &SecretString)
        -> Result<QuotaExtract, ProbeError>;

    /// vendor 是否支持 probe（某些 vendor 没有 dashboard endpoint）。
    fn supports_probe(&self) -> bool { true }
}

pub enum ProbeError {
    /// 上游 4xx（auth 问题，应当让 K-C 处理）
    AuthError,
    /// 上游 5xx / 网络 → 退避
    Transient,
    /// 上游响应里没有解析到 quota 字段（异常协议变化）
    NoData,
}
```

scheduler 启动后周期性遍历所有 (vendor_id, key_id) 对：

```rust
impl QuotaProbeScheduler {
    pub async fn run(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            self.run_one_cycle().await;
        }
    }

    async fn run_one_cycle(&self) {
        for (vendor_id, prober) in self.probers.iter() {
            if !prober.supports_probe() { continue; }
            let interval = self.interval_by_vendor.get(vendor_id)
                .copied()
                .unwrap_or(Duration::from_secs(600));
            let keys = entity_store.list_vendor_keys(vendor_id).await;
            for key in keys {
                if !self.should_probe(&key, interval).await { continue; }
                self.probe_one(&key, prober).await;
            }
        }
    }

    async fn should_probe(&self, key: &VendorKey, interval: Duration) -> bool {
        // 1. 跳过冷却中的 key
        for fmt in all_api_formats() {
            if !self.health_store.is_available_peek(&key.key_id, fmt) { return false; }
        }
        // 2. 距离上次 probe 不到 interval → skip
        let last = key.quota_state.quota_probe_last_attempt_ms;
        if now_ms() - last < interval.as_millis() as u64 { return false; }
        // 3. 连续失败退避
        let fails = key.quota_state.quota_probe_consecutive_fails;
        if fails > 0 {
            let backoff_ms = (1u64 << fails.min(10)) * 1000;  // 指数退避，上限 ~1024s
            if now_ms() - last < backoff_ms { return false; }
        }
        true
    }

    async fn probe_one(&self, key: &VendorKey, prober: &Arc<dyn QuotaProber>) {
        let access_token = self.oauth_cache
            .get_access_token(key, /* ... */).await
            .ok();  // OAuth key 走 K-C；非 OAuth key 用 vendor api_key
        let result = prober.probe(key, &access_token.unwrap_or_else(|| key.api_key.clone())).await;
        match result {
            Ok(extract) => {
                self.quota_writer.write(&key.key_id, extract, QuotaSource::Probe).await;
                self.quota_writer.reset_fail_counter(&key.key_id).await;
            }
            Err(ProbeError::AuthError) => {
                // 交给 K-C 处理；本 scheduler 只 incr fail counter
                self.quota_writer.incr_fail_counter(&key.key_id).await;
            }
            Err(_) => {
                self.quota_writer.incr_fail_counter(&key.key_id).await;
            }
        }
    }
}
```

### ADR-KD-6 启动位置

`QuotaProbeScheduler::run` 是长生命周期 tokio task；与 `gw-ledger::Ledger::start` 同级，在 `gw-gateway::lib::Wired` 装配阶段启动：

```rust
// gw-gateway/src/lib.rs
let quota_scheduler = Arc::new(QuotaProbeScheduler::new(...));
tokio::spawn({
    let s = quota_scheduler.clone();
    async move { s.run().await }
});
```

进程退出时由 graceful shutdown 通知停。

### ADR-KD-7 entity 事件同步

写 `entity:vendor_key.quota_*` 字段后 XADD `events:entity` namespace=`vendor_key` op=`update`。其他副本通过现有 `gw-store::stream_consumer` 接收事件 → invalidate `EntityStore::vendor_key` cache → 下次访问重载。

K-D 不直接对其他副本广播 quota 值——走 entity 协议是足够的。

### ADR-KD-8 routing 集成

`gw-routing::strategy::cost_first` 当前用 pricing 选 key。扩展（**本 spec 不实现**，留 follow-up）：multi-score 策略里加 `quota_balanced` 维度，从 `VendorKey::quota_state.remaining` 读，归一化后做加权。

K-D 本 spec **只**保证字段存在 + 被填写；不动 strategy。

### ADR-KD-9 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_quota_snapshot_total` | vendor, source | counter；source = response_header / response_body |
| `gw_quota_probe_total` | vendor, result | counter；result = ok / auth_error / transient / no_data |
| `gw_quota_probe_skip_total` | vendor, reason | counter；reason = cooldown / interval / backoff |
| `gw_quota_remaining` | vendor, key_id | gauge：当前 remaining；高基数 label 注意采样 |
| `gw_quota_writer_throttled_total` | vendor | counter：被 30s throttle 跳过的写 |

`gw_quota_remaining` 的 `key_id` label 高基数（10000+ key），需要把它做成 vendor 级 histogram（按分位数报告）而非 key 级 gauge。详细方案见未决问题 #2。

## 3. 实施大纲

依赖：K-A land（is_available 接口）；K-C land（OauthCache）；adapter spec ADR-A1（Registry）。

1. 新建 `gw-adapter::quota` 模块（`snapshot.rs` / `probe.rs` / `writer.rs`）；
2. `gw-store::loader` + `gw-core::entity::VendorKey` 加 `QuotaState` 子结构 + entity 字段读写；
3. 实施 `QuotaSnapshotExtractor` trait + Default + 三家示例（Codex / Anthropic / Gemini）；
4. 实施 `QuotaProber` trait（vendor 实现见后续 plan）；
5. 实施 `QuotaProbeScheduler`：`run_one_cycle` 逻辑、退避、跳过冷却；
6. `gw-gateway/src/lib.rs` 装配 + 启动 task；
7. `gw-proxy::filters::response` 在 response 处理路径调 `quota_extractor`；
8. metrics + 测试：
   - 单测每个 extractor（mock headers 输入，断言提取正确）；
   - probe scheduler 在 mock prober + mock entity_store 下的退避正确性；
   - throttle 行为（30s 内只写一次）；
   - entity 事件传播正确性。

vendor probe 实现（Codex GET /usage、Anthropic dashboard、Vertex IAM）作为后续独立 plan。

## 4. 未决问题

1. **probe 频率默认值**：v1 取 10 分钟。某些 vendor（quota 重置周期 24h）可以更长（1h）；某些 vendor（重置 1h）应当 5 分钟。倾向 per-vendor 配置 + default 10 min。
2. **`gw_quota_remaining` 高基数 label**：10000 key × 多 vendor → 几万个 gauge。Prometheus 实践上回避高基数。两个方案：(a) vendor 级 quantile histogram（聚合损失精度），(b) "top-N exhausted" sampler（只暴露最危险的 N 个）。倾向 (a)。
3. **probe 调用是否计入计费**：probe 是空请求或 HEAD，部分 vendor 可能仍计 1 个请求次数。若 quota_unit = Requests 会被自己 probe 消耗——更糟，每次 probe 让 remaining -1。需要 prober 实现自己决定（HEAD vs GET vs 专用 dashboard endpoint）。
4. **被动 snapshot 与主动 probe 的协调**：高 RPM key 不停写 snapshot（throttle 后每 30s 一条），probe 间隔 10 min。两者会冲突吗？倾向：probe 永远只写 "last_source=Probe"，snapshot 写 "last_source=Snapshot"；读端取最新写的（无冲突）。
5. **probe 时机集中风暴**：所有副本启动后同时跑 probe → vendor endpoint 瞬时高负载。需要 random jitter：scheduler 启动时 sleep `Duration::from_secs(rand() % 60)` 错开。
6. **历史 quota 趋势**：v1 只存当前快照；若要做"剩余配额预警"（quota 24h 内消耗加速）需要时间序列。倾向 v1 不做；留 follow-up（可以从 Prometheus histogram 推出来）。
7. **OAuth key vs API key 走不同 prober 入口**：OAuth key 经 `OauthCache::get_access_token` 拿动态 token；API key 用静态 `key.api_key`。`probe` trait 接受统一的 `SecretString` 参数即可，K-D scheduler 内部分流。
8. **manage-time entity 写 vs Rust 写的冲突**：Go 管理面也会写 `entity:vendor_key`（管理面增删改 key）。K-D 写的字段集（`quota_*`）与 Go 写的字段集（基本配置）完全不重叠 → 通过 HSET 部分字段 + version 单调可以共存。但需要在 schema 上 dock 这点，避免后续无意覆盖。

---

> K-D 完成后，桶 3 四个子项全 spec 化。请求路径的精度提升（routing 知道 quota）、运维可观测性（probe metric）、自动恢复（quota 重置后无需失败 retry）三方面均得加强。
