# RL-Adaptive — AIMD Adaptive RPM（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 5 R1）
  - [2026-05-19-ratelimit-rl-core.md](./2026-05-19-ratelimit-rl-core.md)（**硬前置**：RateLimitGuard::override_key_rpm + RateLimitKind）
  - [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md)（K-A cooldown 与本 spec 互补）
  - [2026-05-19-key-health-k-b-vendor-error-hook.md](./2026-05-19-key-health-k-b-vendor-error-hook.md)（K-B ErrorOutcome.rate_limit_info）
  - Aether 来源：`services/rate_limit/adaptive_rpm.py`
- **范围**：在 `gw-auth::ratelimit` 内新增 `AdaptiveRpmManager`：
  - 观察每 vendor_key 的 429 触发频率与上游声明上限；
  - **AIMD 算法**（Additive Increase / Multiplicative Decrease） + **置信度衰减**；
  - 多次观察确认（有 header：2 次；无 header：3 次）；
  - 学到的 RPM 通过 `RateLimitGuard::override_key_rpm` 下调 per-key governor；
  - confidence 低于阈值时停止本地强制（让上游 429 透传，重新学习）。
- **不在范围**：
  - RPM 限速本身（→ RL-Core）；
  - 429 头解析（→ RL-Core::detector）；
  - K-A cooldown（→ K-A）；
  - 任何 user/IP 级别学习（仅 per vendor-key）。

> RL-Adaptive 是"主动降速避免 429"机制，与 K-A "事后冷却" 互补。完成后热点 vendor 上不会出现 429 风暴。

---

## 1. 背景

K-A 的 cooldown 是被动反应：上游返 429 → 标 30s cooldown → 下次请求该 key 被 filter 跳过。问题：

1. **429 不可避免地发生过一次**（用户看到错误或 R-E 切下一 key，浪费请求路径）；
2. **冷却结束后 RPM 仍是配置值**（可能仍超上游真实上限）→ 又 429 → 又 cooldown → 死循环。

Aether 的 AIMD 解决：

- 收 429 + 解析 header 拿"上游真实 RPM = 60"（或推断）；
- **多次观察确认**（header 一致 2 次 / 无 header 推断 3 次）；
- 确认后 `override_key_rpm(50)`（留 buffer）；
- confidence 随时间衰减；长时间无 429 → confidence < threshold → 解除 local override 让上游 RPM 真发挥；
- 若再 429 → 重新学习。

这个机制让 vendor key 在 RPM 边缘附近稳定运行，**避免 429**而非**事后清理**。

## 2. 设计决策

### ADR-RLA-1 数据结构

```rust
// crates/gw-auth/src/ratelimit/adaptive.rs

pub struct AdaptiveRpmManager {
    /// per-key 学习状态
    by_key: DashMap<KeyId, Arc<KeyAdaptiveState>>,
    /// 共享 RateLimitGuard，用于 override_key_rpm
    guard: Arc<RateLimitGuard>,
    /// 后台衰减/扩容任务句柄
    decay_task: Mutex<Option<JoinHandle<()>>>,
}

pub struct KeyAdaptiveState {
    pub key_id: KeyId,
    /// 配置 RPM（VendorKey.rpm_limit；初始 effective_rpm 起点）
    pub configured_rpm: AtomicU32,
    /// 当前已学到的"安全上限"（用作 effective_rpm）
    pub learned_rpm: AtomicU32,
    /// 置信度 [0.0, 1.0]；> threshold 时学到的 RPM 生效
    pub confidence: ArcSwap<Confidence>,
    /// 最近一次观察到的 429 时间
    pub last_429_at_ms: AtomicU64,
    /// 同一上限值的连续观察次数
    pub consistent_observations: AtomicU32,
    /// 最近观察到的上限值（用于一致性比对）
    pub last_observed_limit: AtomicU32,
}

pub struct Confidence {
    pub value: f32,
    pub last_decay_at_ms: u64,
}
```

### ADR-RLA-2 算法常量

```rust
pub struct AdaptiveCfg {
    /// 置信度阈值；高于此值时本地强制 learned_rpm
    pub enforcement_threshold: f32,         // default 0.6
    /// 衰减率（每分钟降低多少 confidence）
    pub confidence_decay_per_min: f32,      // default 0.02
    /// 有上游 header 时的最少一致观察次数
    pub min_header_observations: u32,       // default 2
    /// 无 header 时的最少观察次数（基于本地 RPM 推断）
    pub min_no_header_observations: u32,    // default 3
    /// learned_rpm 留多少 buffer（0.85 = 85% 真实上限）
    pub buffer_ratio: f32,                  // default 0.85
    /// 探测性扩容：距上次 429 超过此时间，可以尝试 +RPM
    pub probe_after_min: u64,               // default 30
    /// 探测性扩容步长（additive increase）
    pub probe_increment: u32,               // default 5
    /// RPM 下限（不会学到比此更低）
    pub min_rpm: u32,                       // default 5
    /// RPM 上限（不会学到比此更高）
    pub max_rpm: u32,                       // default 10000
}
```

默认与 Aether `RPMDefaults` 同步。

### ADR-RLA-3 观察接口

```rust
impl AdaptiveRpmManager {
    /// K-B 在 analyze 路径上把 429 信息传给本管理器。
    /// 调用时机：gw-proxy::filters::logging 在错误响应后调。
    pub fn observe_429(
        &self,
        key_id: &KeyId,
        info: &RateLimitInfo,
        current_local_rpm: u32,
    ) {
        let state = self.get_or_init(key_id);
        let now = now_ms();
        state.last_429_at_ms.store(now, Ordering::Release);

        // 1. 解析"上游真实上限"
        let observed_limit = if let Some(limit) = info.limit_value {
            // header 声明
            limit
        } else {
            // 用 current_local_rpm 作为近似（既然本地这个 RPM 触发了 429，
            // 那真实上限不超过此值）
            current_local_rpm
        };

        // 2. 与上次观察比对
        let last = state.last_observed_limit.load(Ordering::Acquire);
        let consistent = (last == observed_limit) ||
            (last > 0 && (last as i32 - observed_limit as i32).abs() < (last as i32 / 10));
        if consistent {
            state.consistent_observations.fetch_add(1, Ordering::AcqRel);
        } else {
            state.consistent_observations.store(1, Ordering::Release);
        }
        state.last_observed_limit.store(observed_limit, Ordering::Release);

        // 3. 决定是否升 confidence
        let required = if info.limit_value.is_some() {
            self.cfg.min_header_observations
        } else {
            self.cfg.min_no_header_observations
        };
        let obs = state.consistent_observations.load(Ordering::Acquire);
        if obs >= required {
            // 升 confidence
            let mut cur = (*state.confidence.load()).clone();
            cur.value = (cur.value + 0.3).min(1.0);
            cur.last_decay_at_ms = now;
            state.confidence.store(Arc::new(cur));

            // 4. 若 confidence > threshold，强制下调 RPM
            if cur.value >= self.cfg.enforcement_threshold {
                let safe_rpm = ((observed_limit as f32) * self.cfg.buffer_ratio)
                    .max(self.cfg.min_rpm as f32) as u32;
                let safe_rpm = safe_rpm.min(self.cfg.max_rpm);
                state.learned_rpm.store(safe_rpm, Ordering::Release);
                self.guard.override_key_rpm(key_id, safe_rpm);
                tracing::info!(
                    key_id = %key_id.0, rpm = safe_rpm, confidence = cur.value,
                    "adaptive rpm enforced"
                );
                metrics::counter!("gw_adaptive_rpm_enforced_total",
                    "key_id" => key_id.0.clone()).increment(1);
            }
        }
    }

    /// 后台 task 调用：衰减置信度 + 探测性扩容
    pub fn tick_decay(&self) {
        let now = now_ms();
        for entry in self.by_key.iter() {
            let state = entry.value();
            let cur = state.confidence.load();
            let elapsed_min = (now - cur.last_decay_at_ms) as f32 / 60_000.0;
            if elapsed_min < 1.0 {
                continue;
            }
            let new_value = (cur.value - self.cfg.confidence_decay_per_min * elapsed_min).max(0.0);
            let mut new = (**cur).clone();
            new.value = new_value;
            new.last_decay_at_ms = now;
            state.confidence.store(Arc::new(new));

            // 探测性扩容：confidence 已较高但距上次 429 时间长 → 尝试 +RPM
            let since_429 = (now - state.last_429_at_ms.load(Ordering::Acquire)) / 60_000;
            if new_value >= self.cfg.enforcement_threshold &&
               since_429 >= self.cfg.probe_after_min
            {
                let cur_rpm = state.learned_rpm.load(Ordering::Acquire);
                let new_rpm = (cur_rpm + self.cfg.probe_increment).min(self.cfg.max_rpm);
                if new_rpm > cur_rpm {
                    state.learned_rpm.store(new_rpm, Ordering::Release);
                    self.guard.override_key_rpm(&state.key_id, new_rpm);
                    metrics::counter!("gw_adaptive_rpm_probe_total",
                        "key_id" => state.key_id.0.clone()).increment(1);
                }
            }

            // confidence < threshold：解除 local override，让 configured_rpm 复原
            if new_value < self.cfg.enforcement_threshold {
                let cfg_rpm = state.configured_rpm.load(Ordering::Acquire);
                self.guard.reset_key_rpm(&state.key_id);
                state.learned_rpm.store(cfg_rpm, Ordering::Release);
                metrics::counter!("gw_adaptive_rpm_released_total",
                    "key_id" => state.key_id.0.clone()).increment(1);
            }
        }
    }
}
```

### ADR-RLA-4 后台 task

```rust
pub fn spawn_decay_loop(manager: Arc<AdaptiveRpmManager>, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            manager.tick_decay();
        }
    });
}
```

默认 interval 60s。`gw-gateway::lib::Wired` 启动时 spawn。

### ADR-RLA-5 与 RL-Core override_key_rpm 的协作

RL-Core ADR-RLC-6 已声明 `override_key_rpm(key_id, new_rpm)` 与 `reset_key_rpm(key_id)`。RL-Adaptive 是这两个接口的唯一调用方（除测试外）。

VendorKey 配置变更（管理面改 rpm_limit）→ EntityStore 发 invalidation 事件 → RateLimitGuard::reset_key_rpm。RL-Adaptive 的 learned 状态也应当一并清除：

```rust
impl AdaptiveRpmManager {
    pub fn evict(&self, key_id: &KeyId) {
        self.by_key.remove(key_id);
        self.guard.reset_key_rpm(key_id);
    }
}
```

由 EntityStore VendorKey 失效事件触发。

### ADR-RLA-6 RateLimitKind 选择性消费

RL-Adaptive 只关心 `RateLimitKind::Rpm`（与本管理器同维度）。其他 kind：

- `Tpm`：本 spec 不学习（v1 不实现 TPM 限速）；
- `Daily / Monthly`：与 RPM 无关（quota 维度），交给 K-A 长冷却处理；
- `Concurrent`：与并发上限有关；与 stream cap 同维度，交给 RateLimitGuard.per_user_streams；
- `Unknown`：保守处理，与 RPM 同款（按 current_local_rpm 推断）。

```rust
pub fn observe_429(&self, key_id: &KeyId, info: &RateLimitInfo, current_local_rpm: u32) {
    match info.kind {
        RateLimitKind::Rpm | RateLimitKind::Unknown => {
            // 学习
            ...
        }
        _ => {
            // 不学习，但仍记录 last_429_at_ms 影响后续探测
            self.get_or_init(key_id).last_429_at_ms.store(now_ms(), Ordering::Release);
        }
    }
}
```

### ADR-RLA-7 与 K-A cooldown 的边界

K-A 在 429 时也设 cooldown 30s。RL-Adaptive 同时学习 RPM。两者关系：

- K-A：**事中**保护（该 key 30s 内不可选）；
- RL-Adaptive：**事后**学习（30s 后该 key 又可选时，effective_rpm 已下调，避免再 429）；

两者完全互补，无冲突。**RL-Adaptive 不会撤销 K-A cooldown**（cooldown 是 K-A 自管）。

### ADR-RLA-8 性能预算

| 操作 | 预算 |
|---|---|
| `observe_429`（命中） | < 500 ns（几个 atomic + 1 个 ArcSwap） |
| `observe_429`（触发 override） | < 1 µs（额外一次 governor 重建） |
| `tick_decay`（1000 keys） | < 5 ms |
| `evict` | < 200 ns |

observe_429 在错误路径上（已经是 cold path），预算宽裕。tick_decay 1 分钟一次，对热路径无影响。

### ADR-RLA-9 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_adaptive_rpm_observe_total` | key_id, kind | counter：observe 调用 |
| `gw_adaptive_rpm_enforced_total` | key_id | counter：override 触发 |
| `gw_adaptive_rpm_probe_total` | key_id | counter：探测性扩容触发 |
| `gw_adaptive_rpm_released_total` | key_id | counter：confidence 衰减解除强制 |
| `gw_adaptive_rpm_learned_value` | key_id | gauge：当前 learned_rpm（高基数注意） |
| `gw_adaptive_rpm_confidence` | key_id | gauge：当前 confidence |

key_id 标签高基数（10k+ keys）→ 用 vendor 聚合的 histogram 替代 per-key gauge。

### ADR-RLA-10 测试

- 单次 429 + header limit=60 → confidence 不到阈值，不 enforce；
- 连续 2 次 429 + 同 header → confidence > threshold，learned_rpm = 51 (60 × 0.85)，override 触发；
- 连续 3 次 429 无 header → 同上但用 local rpm 推断；
- 不一致观察（55/65）→ consistent_observations 不增长；
- 衰减 5 分钟后 confidence < threshold → release override，rpm 回 configured；
- 探测扩容：confidence 高 + 30 分钟无 429 → learned_rpm + 5；
- 探测扩容后又 429 → 学回 5 buffer；
- evict 后 by_key 表无该 key；
- guard.override_key_rpm 调用次数 = enforced + probe 次数。

## 3. 实施大纲

依赖：RL-Core land（RateLimitGuard + RateLimitInfo）。

1. 新建 `crates/gw-auth/src/ratelimit/adaptive.rs`；
2. `AdaptiveRpmManager / KeyAdaptiveState / Confidence / AdaptiveCfg`；
3. observe_429 / tick_decay / evict 接口；
4. spawn_decay_loop in gw-gateway；
5. K-B `DefaultErrorAnalyzer::analyze` 在 status=429 时调 manager.observe_429；
6. EntityStore VendorKey 失效事件触发 manager.evict；
7. metrics + 测试；
8. 性能 bench（`gw-auth/benches/adaptive_rpm.rs`）。

## 4. 未决问题

1. **buffer_ratio 0.85 是否合理**：留 15% buffer 防止边缘抖动。但某些 vendor 限速窗口非"每 60 秒滑动"而是"整点"，0.85 可能仍触发；可以保守到 0.75。倾向 v1 0.85，monitor enforce → 又 429 的比例决定是否调。
2. **置信度初值**：第一次 observe 时 confidence 从 0 升 0.3。需 2-3 次才到 0.6 阈值。是否要"第一次有 header limit 就立即 enforce"？**不**——单次 429 可能是上游瞬时（系统维护），不能立即学。
3. **enforce 后客户端看到的是什么**：本地 RPM 被下调，超过 learned_rpm 的请求被 RL-Core 拒（GW_QUOTA_RPM_KEY_EXCEEDED）→ R-E SwitchKey。客户端看不到 429（除非所有 key 都 enforce 且全满 → GW_ROUTING_NO_FALLBACK_LEFT）。
4. **跨副本不同步**：与 K-A / R-D 同款；各副本独立学习。代价是冷启动期重复 429 - 各副本独立学一次。可接受。v2 加 Redis pub/sub 同步 learned_rpm；本 spec 不做。
5. **probe_after_min = 30 是否过长**：30 分钟才尝试 +5 RPM 太保守。但激进探测会触发 429 风暴。trade-off 与 buffer_ratio 同源；v1 conservative。
6. **VendorKey rpm_limit = 0（未配置）**：configured_rpm = 0 时 RateLimitGuard 不限速（governor 不存在）。observe_429 时 current_local_rpm 来自哪？倾向用一个全局默认 `unconfigured_rpm_baseline = 100`，监测后 enforce 即写入 learned_rpm，由本管理器持有（governor 在 RL-Core 端建出来）。
7. **TPM 学习何时实现**：v1 不做。但 RateLimitKind::Tpm 已经传过来，RL-Adaptive 可以记录"该 key 触发过 TPM 429"，未来 TPM 限速实现时直接接入。本 spec 不实施 TPM 学习,只暴露 metric `gw_adaptive_rpm_observe_total{kind="tpm"}` 让运维看到。

---

> RL-Adaptive 是桶 5 的"高阶"能力。K-A 是底线（事后清理），RL-Adaptive 是预防（事中学习）。两者完整后热点 vendor 上不再出现 429 风暴。
