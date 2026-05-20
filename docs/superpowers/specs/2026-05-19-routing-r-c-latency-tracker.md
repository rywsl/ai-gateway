# R-C — LatencyTracker(v2026-05-19)

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-routing-r-ab-multi-score.md](./2026-05-19-routing-r-ab-multi-score.md)(LatencyFirst / LoadBalance dimension 消费)
  - [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md)(同款 sliding window 思路)
- **范围**：在 `gw-routing` 内新增 `LatencyTracker`,跟踪每 (KeyId, ApiFormat) 的 TTFT(time to first token)分布;
  - 滑动窗口(默认 300s,容量 1024 样本);
  - 暴露 P50/P95/P99 + sample_count;
  - 由 `gw-proxy::filters::response` 在收到第一个 chunk 时调 `record_ttft(key, fmt, ttft_ms)`;
  - 纯进程内,与 K-A 一样不跨副本同步。
- **不在范围**：
  - 完整 latency 直方图(end-to-end 延迟,与 TTFT 不同);
  - 跨副本 latency 聚合(留 Prometheus dashboard 解决);
  - dimension 实现(已在 R-AB §ADR-RAB-5)。

> R-C 是 R-AB 的数据来源 — latency_first / load_balance dimension 无 LatencyTracker 时 fallback 中性分,land 后真正区分快慢 key。

---

## 1. 背景

R-AB 的 `LatencyFirstDimension` 与 `LoadBalanceDimension` 都需要 `latency_tracker.p50_ttft_ms(key, fmt) -> Option<u32>`。当前 routing 完全没有 latency 数据采集——`gw-routing::strategy::latency_first.rs` 的注释明说 fallback "first available"。

Aether 在 `health/monitor.py` 同时跟踪 health + latency,但 Rust K-A 只承担健康度。把 latency 分到独立 store 的好处:

- 数据维度不同(健康 = 二态 + 计数;latency = 直方图);
- 写入路径不同(健康在 response_filter / error_filter;latency 仅在响应阶段;TTFT 只一次);
- 数据访问频率不同(健康在 candidate filter;latency 仅 dim 算分);

本 spec 把 LatencyTracker 设为独立 store,与 K-A 平级。

## 2. 设计决策

### ADR-RC-1 数据结构

```rust
// crates/gw-routing/src/latency.rs

pub struct LatencyTracker {
    by_key_format: DashMap<(KeyId, ApiFormat), Arc<KeyLatencyState>>,
}

pub struct KeyLatencyState {
    window: Mutex<TtftWindow>,
}

pub struct TtftWindow {
    /// 按时间序的样本(timestamp_ms, ttft_ms)
    samples: VecDeque<(u64, u32)>,
    /// 缓存的最近一次计算结果(避免每次 p50 调用都重排)
    cached_quantiles: Option<CachedQuantiles>,
}

struct CachedQuantiles {
    computed_at_ms: u64,
    p50: u32,
    p95: u32,
    p99: u32,
    count: u32,
}
```

`TtftWindow::samples` 容量 = `WINDOW_CAPACITY`(默认 1024);超容时 pop_front。

### ADR-RC-2 接口

```rust
impl LatencyTracker {
    pub fn record_ttft(&self, key: &KeyId, fmt: ApiFormat, ttft_ms: u32);

    /// 返回 P50 TTFT,样本不足 N 时返回 None(由 dim 兜底中性)。
    pub fn p50_ttft_ms(&self, key: &KeyId, fmt: ApiFormat) -> Option<u32>;
    pub fn p95_ttft_ms(&self, key: &KeyId, fmt: ApiFormat) -> Option<u32>;
    pub fn p99_ttft_ms(&self, key: &KeyId, fmt: ApiFormat) -> Option<u32>;

    /// 完整 snapshot(用于 metrics / debug)
    pub fn peek(&self, key: &KeyId, fmt: ApiFormat) -> Option<LatencySnapshot>;

    /// 驱逐某 key(配合 entity 失效事件)
    pub fn evict(&self, key: &KeyId);
}

pub struct LatencySnapshot {
    pub sample_count: u32,
    pub p50_ms: u32,
    pub p95_ms: u32,
    pub p99_ms: u32,
    pub last_record_ms: u64,
}
```

样本量阈值:`MIN_SAMPLES_FOR_QUANTILE = 5`。少于 5 个样本返回 None(避免初始几次请求支配 quantile)。

### ADR-RC-3 quantile 计算策略

每次 `p50_ttft_ms` 调用时若需要 quantile:

- 检查 `cached_quantiles.computed_at_ms` 是否在 `RECOMPUTE_INTERVAL_MS = 5000`(5 秒) 内;
- 若是 → 直接返回 cache;
- 否 → 排序 + 重算 + 更新 cache;

cost: 排序 1024 元素 < 30 µs。5 秒缓存意味着 latency dimension 在 5 秒内对同一 key 拿同一 P50,这对调度决策影响极小(latency 本身就是慢变量)。

### ADR-RC-4 滑动窗口裁剪

`record_ttft` 时:
1. push_back (now_ms, ttft_ms);
2. 驱逐 (now_ms - WINDOW_DURATION_MS) 之前的样本;
3. 若 samples.len() > WINDOW_CAPACITY → pop_front 直到 == CAPACITY;
4. 失效 cached_quantiles(下次读触发重算)。

默认 `WINDOW_DURATION_MS = 300_000`(5 分钟);`WINDOW_CAPACITY = 1024`。

### ADR-RC-5 接入点

`gw-proxy::filters::response` 在收到上游响应第一个 body chunk 时:

```rust
async fn response_body_filter(...) {
    if !ctx.ttft_recorded && body_starts_with_useful_data(body) {
        let ttft_ms = ctx.timing.start
            .and_then(|s| ctx.timing.ttft.map(|t| t.duration_since(s).as_millis() as u32))
            .unwrap_or(0);
        if ttft_ms > 0 {
            ctx.latency_tracker.record_ttft(&ctx.vendor_key_id, ctx.api_format, ttft_ms);
        }
        ctx.ttft_recorded = true;
    }
    // ...
}
```

非流响应:`response_filter` EOS 时 record(整 body 完整收到时算 ttft)。

错误请求(4xx/5xx)**不** record TTFT——错误响应的"首字节时间"不能代表正常请求性能。

### ADR-RC-6 多副本独立

与 K-A 同款:每个 Rust 副本各自维护 LatencyTracker;副本启动后样本为空,前 5 个请求 dim 返回中性 → 5 个请求后逐渐学到差异。

这是 trade-off:**接受冷启动期 latency dim 不准**,换取热路径零 Redis 调用。

跨副本 latency 聚合靠 Prometheus(每副本 emit `gw_latency_ttft_ms` histogram,Grafana 端聚合)——但这是观测用,不参与调度。

### ADR-RC-7 entity 失效集成

VendorKey 被删/旋转时 EntityStore 发 invalidation 事件 → `LatencyTracker::evict(&key_id)` 清掉该 key 的所有 ApiFormat 桶。

### ADR-RC-8 性能预算

| 操作 | 预算 |
|---|---|
| `record_ttft` | < 200 ns(Mutex lock + push + cache invalidate) |
| `p50_ttft_ms` cache hit | < 50 ns |
| `p50_ttft_ms` cache miss + 重算 1024 样本 | < 30 µs |
| `peek` | < 200 ns(读 cached_quantiles) |
| `evict` | < 100 ns(DashMap remove) |

### ADR-RC-9 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_latency_ttft_ms` | vendor_id, fmt | histogram:每次 record_ttft 的样本(用于跨副本聚合) |
| `gw_latency_tracker_samples` | key | gauge:当前样本数(高基数,可选) |
| `gw_latency_quantile_recompute_total` | - | counter:cache miss 触发重算次数 |
| `gw_latency_evict_total` | - | counter:entity 失效驱逐次数 |

### ADR-RC-10 测试

- record 1000 样本 → P50/P95/P99 与 percentile 库结果一致(±1ms);
- 样本 < 5 → p50_ttft_ms 返回 None;
- 老样本(超 5 分钟) 自动驱逐;
- 容量上限 1024 → 第 1025 个 push 后 popleft;
- cache 5s 内重复读不重算;
- 并发:8 个线程 record 不同 key + 主线程 read,无 race。

## 3. 实施大纲

依赖:R-AB land(PickCtx 接口已声明)。

1. 新建 `gw-routing/src/latency.rs`;
2. 实施 `LatencyTracker / KeyLatencyState / TtftWindow / CachedQuantiles`;
3. `gw-proxy::ctx::Ctx` 加 `latency_tracker: Arc<LatencyTracker>` 字段;
4. `gw-proxy::filters::response` 接入 record_ttft;
5. EntityStore invalidation 触发 evict;
6. 单测覆盖 + 性能 bench(`gw-routing/benches/latency_tracker.rs`)。

## 4. 未决问题

1. **窗口配置可调性**: 5min / 1024 samples / min 5 / recompute 5s 默认值——是否需要 env / config 暴露?倾向 v1 const + env override(GW_LATENCY__WINDOW_SECS 等),后续按需细化。
2. **error 与 success 是否同窗口**: 现状 5xx 不 record。但是 5xx 响应往往很慢(server timeout),不 record 可能让该 key 看起来还很快 → dim 继续选它。倾向 v1 不 record 5xx;后续考虑加单独 error_latency_tracker 让 K-A 用。
3. **TTFT vs 全 body latency**: stream 用 TTFT(首字节);非流响应只有"end of response" 一个时间点,把它当 TTFT?倾向是,非流响应的 TTFT = 整体 latency,语义上仍能反映"key 多快"。
4. **冷启动 vs warm 期权重调整**: 样本 < 50 时 dim 是否给中性 0.5?目前 < 5 给 None。如果 5~50 个样本时直接用 P50 可能噪声大。倾向加 `WARMUP_THRESHOLD = 50`,5~50 之间用 P50 但 dim 内部把分数从 1.0 衰减到 0.5(给 warmed key 完全的偏好,给冷启动 key 部分偏好)。这条放 dim 内部决定,LatencyTracker 只提供原始 quantile。
5. **直方图算法**: 当前每次重排 1024 样本算 quantile,简单但 O(N log N)。可换 t-digest(更快近似,常数空间),但 1024 样本 30 µs 已经够快,不值得引入依赖。
6. **多 ApiFormat 数据共享**: 同一 key 在 OpenAI 与 Anthropic 上的 TTFT 可能有系统性差异(Anthropic 协议 overhead 更大)。所以按 (KeyId, ApiFormat) 分桶。memory 略增(同 key × 多 fmt) 但精度高。

---

> R-C 是个独立、薄、清晰的 spec。完成后 R-AB 的 latency / load_balance dim 真正能区分快慢 key。
