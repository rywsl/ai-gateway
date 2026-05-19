# R-E — Failover 执行层(v2026-05-19)

- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-routing-r-ab-multi-score.md](./2026-05-19-routing-r-ab-multi-score.md)(**硬前置**:SelectorAny::pick 返回 SmallVec)
  - [2026-05-19-routing-r-f-stream-probe.md](./2026-05-19-routing-r-f-stream-probe.md)(stream 路径 failover 配套)
  - [2026-05-19-key-health-k-b-vendor-error-hook.md](./2026-05-19-key-health-k-b-vendor-error-hook.md)(error 分类)
  - 现状参考:`gw-routing::retry`
- **范围**：把"选 key + retry"统一为 failover 调度:
  - 执行层(`gw-proxy::filters::upstream` 与未来 `gw-task-runtime::executor`) 消费 `SelectorAny::pick` 的排序列表;
  - 按列表顺序**逐个尝试** vendor_key,直到成功或全部失败;
  - retry 不再"自行决定换不换 key",而是按 list 顺序走;
  - K-B 错误分类决定:同 key 重试(transient 5xx)、换 key(403/账号问题)、立即报错(4xx terminal);
  - 全局 retry 预算(时间 / 次数);
  - 全部 fallback 失败时 emit 统一错误码 `GW_ROUTING_NO_FALLBACK_LEFT`。
- **不在范围**：
  - Strategy::pick 内部(→ R-AB);
  - 流式响应中途重试(→ R-F stream probe);
  - 重试在 stream 已开始后不允许的语义(本 spec 沿用现 retry 约束)。

> R-E 把当前"strategy 选一个 + retry 自决定"模型升级为"strategy 给排序 + 执行层按顺序尝试"。这是 Aether `candidate/failover.py` 等价。

---

## 1. 背景

当前 `gw-routing::retry::RetryExecutor::should_retry(err, bytes_sent)` 决定:

- err.retryable && bytes_sent == 0 → retry yes
- 非幂等 + non-timeout → retry no
- 流式中 bytes_sent > 0 → retry no

retry **同 key** 还是 **换 key** 由 `RetryMode` 控制(SameKey / CrossKey / Mixed)。但 CrossKey 的"下一个 key 选谁"是模糊的:Strategy::pick 只返回首选;retry 想换 key 时再次调 strategy.pick? 那 strategy 怎么知道"上次选过哪个,这次别选了"?

当前实际:`gw-proxy::filters::upstream` 第一次 pick 后存 ctx.picked_vendor_key;retry 时再 pick 一次 — 但 strategy 没有"排除集"参数,可能选同一个。

R-AB 改 pick 返回 SmallVec 排序列表,把这个问题彻底解决:执行层拿到完整 list 后**按顺序**走,strategy 不参与"该不该换"决策。

## 2. 设计决策

### ADR-RE-1 FailoverExecutor 抽象

```rust
// crates/gw-routing/src/failover.rs

pub struct FailoverExecutor {
    pub config: FailoverConfig,
}

pub struct FailoverConfig {
    /// 全局重试预算(毫秒);跨所有 vendor_key 累计。
    pub total_budget_ms: u32,
    /// 单 key 最多重试次数(transient 5xx 同 key 重试)。
    pub per_key_max_retries: u32,
    /// 跨 key fallback 最多次数。
    pub max_failover_keys: u32,
    /// 是否在 stream 已开始后允许重试(默认 false)。
    pub allow_stream_retry: bool,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            total_budget_ms: 60_000,
            per_key_max_retries: 2,
            max_failover_keys: 5,
            allow_stream_retry: false,
        }
    }
}
```

### ADR-RE-2 单次请求的 failover 状态机

```rust
pub struct FailoverState {
    pub candidates: SmallVec<[Arc<VendorKey>; 8]>,  // 来自 SelectorAny::pick
    pub current_idx: usize,
    pub started_at: Instant,
    pub same_key_retries: u32,
    pub bytes_sent_to_client: u64,
    pub last_error: Option<ErrorIR<'static>>,
}

impl FailoverState {
    pub fn from_picks(picks: SmallVec<[Arc<VendorKey>; 8]>) -> Self {
        Self {
            candidates: picks,
            current_idx: 0,
            started_at: Instant::now(),
            same_key_retries: 0,
            bytes_sent_to_client: 0,
            last_error: None,
        }
    }

    pub fn current(&self) -> Option<&Arc<VendorKey>> {
        self.candidates.get(self.current_idx)
    }

    /// 决定下一步:同 key 重试 / 切下一 key / 终止。
    pub fn next_action(&mut self, err: &ErrorIR, cfg: &FailoverConfig) -> FailoverAction {
        if self.started_at.elapsed().as_millis() as u32 >= cfg.total_budget_ms {
            return FailoverAction::Exhausted(ExhaustedReason::Timeout);
        }
        if self.current_idx >= cfg.max_failover_keys as usize {
            return FailoverAction::Exhausted(ExhaustedReason::MaxFailoverKeys);
        }
        if !cfg.allow_stream_retry && self.bytes_sent_to_client > 0 {
            return FailoverAction::Exhausted(ExhaustedReason::StreamAlreadyStarted);
        }

        match classify_for_failover(err) {
            FailoverDecision::RetrySameKey if self.same_key_retries < cfg.per_key_max_retries => {
                self.same_key_retries += 1;
                FailoverAction::SameKey
            }
            FailoverDecision::RetrySameKey => {
                // 同 key 上限达成,切下一个
                self.advance_to_next_key();
                FailoverAction::NextKey
            }
            FailoverDecision::SwitchKey => {
                self.advance_to_next_key();
                FailoverAction::NextKey
            }
            FailoverDecision::FailImmediately => {
                FailoverAction::Exhausted(ExhaustedReason::NonRetryableError)
            }
        }
    }

    fn advance_to_next_key(&mut self) {
        self.current_idx += 1;
        self.same_key_retries = 0;
    }
}

pub enum FailoverAction {
    /// 同 key retry(transient 5xx / 网络瞬时)
    SameKey,
    /// 切到列表下一 key
    NextKey,
    /// 全部失败/不可重试,emit 错误
    Exhausted(ExhaustedReason),
}

pub enum ExhaustedReason {
    Timeout,
    MaxFailoverKeys,
    StreamAlreadyStarted,
    NonRetryableError,
    NoMoreCandidates,
}
```

### ADR-RE-3 错误分类决定 action

```rust
pub enum FailoverDecision {
    /// 同 key 短期重试(5xx 瞬时、网络瞬时)
    RetrySameKey,
    /// 切下一 key(账号问题、403、401、quota 临时耗尽)
    SwitchKey,
    /// 立即报错(4xx terminal:bad request、不支持的模型、内容审核)
    FailImmediately,
}

pub fn classify_for_failover(err: &ErrorIR) -> FailoverDecision {
    if !err.retryable { return FailoverDecision::FailImmediately; }
    match err.status {
        // 5xx 瞬时
        500..=599 => FailoverDecision::RetrySameKey,
        // 网络瞬时(代码用 0/连接错误)
        0 => FailoverDecision::RetrySameKey,
        // 鉴权 / 账号问题:key 已经死了,换
        401 | 402 | 403 => FailoverDecision::SwitchKey,
        // quota
        429 => {
            // 已被 K-B 喂 cooldown,该 key 下次自动被 filter 排除;
            // 本次切下一 key
            FailoverDecision::SwitchKey
        }
        // 4xx terminal
        400 | 404 | 422 => FailoverDecision::FailImmediately,
        _ => FailoverDecision::SwitchKey,
    }
}
```

注意:K-B 的 error_analyzer 已经在 logging 路径上把 cooldown 写到 KeyHealthStore;下次 list_candidates 时该 key 自动 filter 掉。R-E 本次请求的 SwitchKey 立即生效不等下次 — 因为 ctx.candidates 在请求开始时一次性 pick,中途不重 pick(否则破坏稳定性)。

### ADR-RE-4 retry executor 撤销

`gw-routing::retry::RetryExecutor::should_retry` 当前的逻辑全部并入 `FailoverState::next_action`。RetryConfig 字段:
- `mode: RetryMode { SameKey / CrossKey / Mixed }` — 撤销(FailoverConfig 没这个开关,默认 SameKey transient + SwitchKey 4xx)
- `total_budget_ms_*` — 合并到 FailoverConfig.total_budget_ms
- `max_attempts` — 合并到 per_key_max_retries

整个 `gw-routing/src/retry.rs` 撤销;接口由 `gw-routing/src/failover.rs` 取代。

### ADR-RE-5 gw-proxy 集成

```rust
// gw-proxy::filters::upstream(伪代码,实际是异步分阶段)
async fn upstream_peer(&self, ctx: &mut ReqCtx, session: &Session) -> Result<Box<HttpPeer>> {
    if ctx.failover.is_none() {
        // 首次:list + pick + 装填 FailoverState
        let candidates = list_candidates(&group, &ctx.candidate_ctx, &store, &chain).await;
        let picks = ctx.selector.pick(&candidates, &ctx.pick_ctx);
        if picks.is_empty() {
            return Err(emit_error(ctx, codes::GW_ROUTING_NO_AVAILABLE_CHANNEL));
        }
        ctx.failover = Some(FailoverState::from_picks(picks));
    }

    let state = ctx.failover.as_mut().unwrap();
    let key = state.current().ok_or_else(|| emit_error(ctx, codes::GW_ROUTING_NO_FALLBACK_LEFT))?;
    Ok(Box::new(http_peer_for(key)))
}

// 在 fail_to_proxy / on_upstream_error 时调:
async fn on_upstream_error(&self, ctx: &mut ReqCtx, err: &ErrorIR<'_>) -> ControlFlow {
    let state = ctx.failover.as_mut().expect("failover state must exist");
    state.last_error = Some(err.clone_static());
    match state.next_action(err, &self.failover_cfg) {
        FailoverAction::SameKey => ControlFlow::Retry,
        FailoverAction::NextKey => ControlFlow::Retry,
        FailoverAction::Exhausted(reason) => {
            let final_err = build_exhausted_error(reason, state.last_error.as_ref());
            ControlFlow::Bail(final_err)
        }
    }
}
```

Pingora `fail_to_proxy` 回调对应 `on_upstream_error`;`ControlFlow::Retry` 让 Pingora 重新调 `upstream_peer`(读到新 current_idx)。

### ADR-RE-6 流式语义

`allow_stream_retry: false`(默认) → 一旦 `bytes_sent_to_client > 0`,任何失败都 Exhausted::StreamAlreadyStarted。**这是为了避免客户端看到"半截 SSE + 重新开始的 SSE"——上游中断流后切换看似可行,但客户端 SDK 看到两个 message_start 会崩。**

R-F(stream probe)在"客户端还没收到任何字节"窗口期重试 — 那是 `bytes_sent_to_client == 0` 但上游已开始 stream 的场景,本 spec 完全允许(走 SwitchKey/SameKey),R-F 是这个机制的特殊化。

### ADR-RE-7 错误码

新增 `gw-core::error::codes`:

- `GW_ROUTING_NO_FALLBACK_LEFT`(503):候选列表全失败 + reason=NoMoreCandidates / MaxFailoverKeys;
- `GW_ROUTING_FAILOVER_TIMEOUT`(504):total_budget_ms 用尽;
- `GW_ROUTING_STREAM_NOT_RETRIABLE`(502):stream 已发字节后失败,无法切换;

`build_exhausted_error` 把 reason 映射到对应码;原始 last_error 作为 nested error 附加。

### ADR-RE-8 可观测性

| Metric | Label | 说明 |
|---|---|---|
| `gw_failover_attempt_total` | reason | counter:每次 attempt 触发原因(initial / same_key_retry / next_key) |
| `gw_failover_exhausted_total` | reason | counter:Exhausted 各种原因 |
| `gw_failover_keys_used` | - | histogram:单请求用了几个 key(分位数) |
| `gw_failover_total_duration_ms` | result | histogram |
| `gw_failover_classify_total` | decision, status | counter:错误分类输出 |

detailed_log(ADR-026) 新增字段:
- `failover.candidates_count`:列表长度;
- `failover.keys_tried`:实际尝试 ids;
- `failover.exhausted_reason`:终止原因(成功时 None)。

### ADR-RE-9 配置

`FailoverConfig` 通过 ModelGroup entity 注入,允许 per-ModelGroup 调优:

```rust
pub struct RoutingConfig {
    pub selector: SelectorKind,
    pub multi_score: Option<HashMap<String, f32>>,
    pub failover: FailoverConfig,  // ← 新加
}
```

全局 fallback 默认值 `FailoverConfig::default()`。

### ADR-RE-10 性能预算

| 操作 | 预算 |
|---|---|
| `FailoverState::from_picks` | < 100 ns(SmallVec clone) |
| `FailoverState::next_action` | < 100 ns(几个 atomic compare) |
| `classify_for_failover` | < 50 ns |

failover 本身不在 hot path 上;hot path 是首次 upstream_peer 成功的情况(95%+ 请求)。

### ADR-RE-11 测试

- list 2 个 key,首个 5xx → SameKey retry × 2(达到 per_key_max_retries)→ NextKey → 第二个 200 → 成功;
- list 2 个 key,首个 401 → SwitchKey 直接换;第二个 200;
- list 1 个 key,5xx 用完 retry → Exhausted::MaxFailoverKeys → GW_ROUTING_NO_FALLBACK_LEFT;
- total_budget_ms 超时 → Exhausted::Timeout → GW_ROUTING_FAILOVER_TIMEOUT;
- stream 中已发字节后 5xx → Exhausted::StreamAlreadyStarted → GW_ROUTING_STREAM_NOT_RETRIABLE;
- 400 bad request → 立即 FailImmediately,不切;
- classify 各 status 码 → 决策正确;
- detailed_log keys_tried 记录正确。

## 3. 实施大纲

依赖:R-AB land(SelectorAny);K-B land(error_analyzer 已 settle KeyHealth)。

1. 新建 `gw-routing/src/failover.rs`(FailoverConfig / FailoverState / FailoverDecision / classify);
2. 撤销 `gw-routing/src/retry.rs`(导出兼容 stub 一个版本周期);
3. `gw-proxy::ctx::Ctx` 加 `failover: Option<FailoverState>` 字段;
4. `gw-proxy::filters::upstream::upstream_peer` 接入 FailoverState;
5. `gw-proxy::gateway::on_upstream_error` 路径接入 next_action;
6. 错误码登记;
7. detailed_log schema 扩字段;
8. metrics + 测试。

## 4. 未决问题

1. **同 key retry 的 backoff**: per_key_max_retries 是次数,但没规定每次间隔。当前 v1 立即重试 — 上游 5xx 通常需要 100ms-1s 退避才能恢复。倾向加 `same_key_backoff_ms`(默认 200ms 起,指数),但写本 spec 时性能影响要测。倾向 v1 100ms 固定,observe 决定。
2. **per-(KeyId, ApiFormat) 重试上限**: 同 key 多 ApiFormat 时 retry budget 应该共享还是独立?倾向独立 — 同 key 在 OpenAI 上 5xx 不影响其在 Anthropic 上的剩余重试预算。
3. **被 K-A cooldown 后是否要从 list 中移除**: list 在 pick 时已经过滤了 cooldown 的 key;但 list 中第 1 个 key 失败时被 K-B cooldown 标了,继续 NextKey 切到第 2 个 — 不会再触它。问题是第 2 个 key 在 list 选取时被认为可用,但如果 K-A 此时也标了它呢?**不重新走 pick 是设计选择**,接受少量浪费(本请求路径继续打"已 cooldown 但 list 内"的 key)。下次新请求会重 pick。
4. **request body 多 key 复用**: 跨 key fallback 时需要重发 body。Pingora 默认允许(retry path 重读 cached body)。但 stream upstream body 不能 rewind — 与 v1 "stream 不重试" 约束一致。
5. **fixed_order strategy 与 failover 冲突?**: FixedOrder 输出排序列表,failover 按顺序走 — 等价。但 RoundRobin 输出"轮询第 N 个为首"的列表,fallback 是"剩下的"?或者 round_robin 输出确定列表?倾向后者(R-AB §ADR-RAB-4 的 selectorAny 已声明 picks 是 SmallVec)。
6. **stream probe (R-F) 与 R-E 的边界**: R-E 的 allow_stream_retry=false 默认对 R-F 来说是放行的(因为 bytes_sent_to_client == 0);R-F 加一层"如果首 chunk 已确认到达客户端则锁定"的判断 — 这是 R-F 自己的事,R-E 不需要变。
7. **错误 cloned_static**: `state.last_error: ErrorIR<'static>` 要 clone,但 ErrorIR 含 Cow 字段,需要 `to_static()` 方法。需要在 gw-core::error 实现该方法(可能已有,确认下)。

---

> R-E 把 strategy 与 retry 解耦,failover 成为执行层独立模块。R-F 在此之上加 stream 安全网。
