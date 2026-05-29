# 计费鲁棒性统一设计(billing robustness)— v2026-05-24

- **日期**:2026-05-24
- **状态**:Draft(待审阅)
- **作者**:协作设计(Claude + 用户)
- **触发**:用户指出当前计费链路存在多处鲁棒性缺口,具体痛点 — 客户端取消 stream 后网关未同步取消上游、partial usage 未按真实计费、cache_creation/thinking/multimodal 三个维度被写死 `None` 在静默漏钱、Rust 崩溃窗未闭环。要求一次性出统一设计,不接受逐点打补丁。
- **关联**:
  - [`docs/architecture.md`](../../architecture.md) §3(`gw-ledger` / `gw-proxy`)、§5(Redis namespace)、§7(invariants)、§Deliberate behaviours — 本 spec 触发更新
  - [`docs/workflow.md`](../../workflow.md) §filter chain `response_body_filter` / `logging` — 本 spec 触发重写
  - [`docs/go-contracts/parts/02-go-as-consumer.md`](../../go-contracts/parts/02-go-as-consumer.md) §2.1 `usage:records` — 本 spec 触发新增去重 contract
  - [`docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md`](2026-05-23-v2-cleanup-overview-design.md) §S-ledger、§S-proxy-core — 本 spec **吸纳进**两者,不另起独立 sub-spec(分配见 §8)
  - 既有计费规约:[`2026-05-19-billing-c-alpha-usage-record.md`](2026-05-19-billing-c-alpha-usage-record.md)(UsageRecord schema)、[`2026-05-19-billing-c-gamma-balance-split.md`](2026-05-19-billing-c-gamma-balance-split.md)(freeze / release / DEDUCT 分工)、[`2026-05-19-billing-c-delta-stream-dlq.md`](2026-05-19-billing-c-delta-stream-dlq.md)(DLQ)— 本 spec 在它们之上加生命周期 + 取消传播 + 崩溃恢复
- **范围**:
  - 计费 lifecycle 状态机化(`VendorExposure` / `ClientDelivery` 双状态机)
  - `UsageExtractor` 6 维 × 3 codec 提取矩阵全量落实(消除当前写死 `None`)
  - `UpstreamCancellationCoordinator`:客户端断开 / 网关 deadline / 上游异常 统一处理 + grace timer 抓 final usage
  - `InflightLedger`:Redis HASH 记录 in-flight 计费状态,支持崩溃恢复 + T0 emit-lag 探测
  - `AttemptManifest`:多 vendor key 重试时每 attempt 独立 UsageRecord
  - `UsageRecord` schema 一次性破坏性升级(新增 `attempt_index` / `vendor_exposure_state` / `usage_provenance`,移除 `usage_source`)
  - `UsageRecordEmitter` must-deliver 加固(与 `usage:detailed` 的 drop-on-outage 严格区分)
- **不在范围**:
  - cost 计算(`no_cost_micro` invariant,Go 侧职责)
  - Go billing settler 内部公式 / 对账逻辑
  - 用户层 balance 显示策略
  - tokenizer 选型(prompt_estimate 来源沿用现有 `gw-core/src/tokenizer`)
  - Cross-protocol IR 重设计(本 spec 不动 codec IR,只动 usage 提取)
  - WebSocket tunnel 子系统的计费侧(沿用现有 vendor key 抽象,tunnel preference 不影响 BillingTransaction 模型)

> **一句话定义**:每个抵达 upstream wire 的请求生成一个或多个 `BillingTransaction`,其 `VendorExposure` 状态机一旦进入 `Sent` 就必须最终产生一条 `UsageRecord`,不论客户端是否完整接收、上游是否成功、Rust 进程是否崩溃。

---

## 1. 现状盘点

### 1.1 当前漏点

| # | 位置 | 现象 | 影响 |
|---|---|---|---|
| L1 | `crates/gw-proxy/src/filters/logging.rs:204-207` | `cache_creation_input_tokens` / `thinking_tokens` / `multimodal_input_tokens` 三字段写死 `None` | Anthropic 写缓存 token、o1/Claude extended-thinking、Gemini 多模态 input — 全部 0 计费,持续漏钱 |
| L2 | `crates/gw-proxy/src/filters/logging.rs:209-213` | `usage_source = if output_tokens > 0 { Upstream } else { Estimated }` | 客户端在第一个 chunk 前断开 → `output_tokens=0` → 标 `Estimated`。但若上游已收 prompt,真实情况应是"`Upstream` + 真 input + output=0",不是退化到估算 |
| L3 | (无)取消传播 | 客户端 TCP close / HTTP/2 RST 后,上游是否同步 cancel 未定义;部分情况上游继续生成、计费但下游早断 | 用户侧体感"已取消"、厂商扣全额、网关按估算结算 — 三方账不平 |
| L4 | `crates/gw-ledger/src/freeze.rs` `g.release()` | freeze 单向释放,actual usage 的 delta DEDUCT 完全交给 Go settler;Rust 侧没有保险栅检测 `usage:records` 是否真到达 Go | 若 emit pipeline 卡死,freeze 已 release、扣款没发生,balance 错账 |
| L5 | (无)崩溃恢复 | Rust 进程在 `Sent` 与 emit 之间崩溃,该次 vendor 扣费对 Go 不可见 | DLQ 只覆盖 XADD 失败,不覆盖 Rust 进程崩溃。生产偶发 OOM/panic 后该笔账永远丢 |
| L6 | (无)重试 schema 支持 | `UsageRecord` 1:1 `vendor_key_id`,无 `attempt_index`。若路由层失败重试,前一个 vendor key 已收 input 也已扣费,但只有最后一个 attempt 进 `usage:records` | 多 key 路由时漏算前序失败 attempt 的 vendor 实际扣费 |
| L7 | response deadline | `S-proxy-core` 已列入待实现项,但当前实现没有任何上游响应超时机制 | 上游 hang → freeze 永不 release → 用户额度永久占用 |

### 1.2 现有可复用资产

- `gw-core::usage::UsageSnapshot` schema 6 维已就位(`crates/gw-core/src/usage.rs:11-19`),仅缺 codec 端提取
- `gw-ledger::freeze::FreezeGuard` 已支持 release 语义,扩展承载 lifecycle 钩子
- `gw-ledger::flusher` + `usage:records` + `usage:records:dlq` 已闭环 XADD 失败路径
- `crates/gw-proxy/src/ctx.rs::ReqCtx` 已具备 `usage`、`freeze_guard`、`retry_count`、`bytes_sent_to_client` 等字段,可作为 BillingTransaction 状态机承载
- detailed-log 系统(spec `2026-05-23-detailed-log-complete-design.md`)已落地 5-surface 触发机制,本 spec 的 `VendorExposureTracker` 可复用其触发节点

---

## 2. 不变量

> **如果上游厂商已对网关托管账户产生扣费,网关必须按真实 token 6 维(input / output / cache_read / cache_creation / thinking / multimodal)向用户出账,不论客户端是否完整接收、上游是否成功、Rust 进程是否崩溃。**

推论:
- 计费可靠性的边界 = 上游"是否被扣"的边界,**不**等同于"客户端是否拿到结果"的边界
- 这两个边界必须解耦 — 当前漏点 L2 / L3 全部来自这个耦合

---

## 3. 心智模型:BillingTransaction

每个抵达 upstream wire 的请求生成 **1+ 个** `BillingTransaction`(重试时 > 1):

```rust
struct BillingTransaction {
    id: (RequestId, AttemptIndex),
    vendor_key_id: KeyId,
    exposure: VendorExposure,        // §4.1 状态机
    usage: UsageSnapshot,            // 6 维, 增量填
    provenance: UsageProvenance,     // 每维独立来源标签
    freeze_ref: Option<FreezeGuard>,
}
```

**生命周期保证**:一旦进入 `Sent`,必须最终产生一条 `UsageRecord` 写入 `usage:records`,不论后续路径如何。

`BillingTransaction` 不是新数据结构,而是 `ReqCtx` 上若干字段的逻辑分组(见 §4.1 详细映射)。

---

## 4. 两条独立状态机

### 4.1 VendorExposure(决定欠厂商多少)

```text
       Idle
        │  upstream_request_filter 末尾把 bytes 交给 pingora
        ▼
       Sent ────────► [InflightLedger HSET]
        │  response_filter 收到第一个 byte
        ▼
    Streaming
       │ │
       │ └─── cancel trigger (client close / 网关 deadline / 上游 RST / 上游 5xx)
       │                              │
       │                              ▼
       │  [UpstreamCancellationCoordinator 内部: 关上游 + grace 抓 final usage,
       │   grace 期 = `cancel_grace_ms`(默认 200, 可配)— 详见 §5.3]
       │                              │
       │                              ▼
       │                          Terminated
       │
       └─── final usage event / non-stream EOS ─────► Settled
                                                       │
                                  Settled/Terminated ──┤
                                                       ▼
                                            [emit UsageRecord]
                                            [InflightLedger DEL]
```

转移**不可逆**。每个 attempt 持有独立状态机,重试⇒新 `attempt_index`⇒新 `BillingTransaction`。

**状态层级**:in-RAM `VendorExposure` 5 状态(`Idle / Sent / Streaming / Settled / Terminated`)是 lifecycle 内部使用;持久化进 `UsageRecord.vendor_exposure_state` 的只有 3 个终态值(`Settled / Terminated / CrashFallback`,见 §6)— in-flight 状态永不 emit,因 emit 只在到达终态后发生。**cancel 期间的 grace 处理是 `UpstreamCancellationCoordinator` 的内部行为**,不出现在 `VendorExposure` 枚举里 — 状态机对调用方只暴露 `Streaming → Terminated` 的直接转移。

**ReqCtx 字段映射**:
- 状态:`ReqCtx.vendor_exposure: VendorExposure` (新增, 5 状态枚举)
- `ts_sent_ms`:`ReqCtx.timing.sent_ms` (新增)
- `ts_settled_ms`:`ReqCtx.timing.settled_ms` (新增)
- `attempt_index`:`ReqCtx.attempt_index: u32` (新增,替代 `retry_count` 的语义)

### 4.2 ClientDelivery(独立子系统,与计费解耦)

```text
  Connected ── client TCP close / HTTP-2 RST ──► Disconnected
                                                       │
                                                       ▼
                                    [仅作为 UpstreamCancellationCoordinator
                                     的 cancel trigger 之一]
                                    [不影响 UsageRecord 最终落地]
```

`ClientDelivery::Disconnected` **只**作为 cancel trigger 之一(与 deadline、upstream RST 并列),不直接参与计费决策。

---

## 5. 子系统设计

### 5.1 VendorExposureTracker

挂在 `ReqCtx` 上,在 4 个 filter 节点写状态:

| Pingora filter | 转移 | 实施位置 |
|---|---|---|
| `upstream_request_filter` 末尾 | `Idle → Sent` + `InflightLedger.register()` | `crates/gw-proxy/src/filters/upstream.rs` |
| `response_filter` 第一个 body byte 到达 | `Sent → Streaming` | `crates/gw-proxy/src/filters/response.rs` |
| `response_body_filter` 解析到 final usage event / non-stream EOS | `Streaming → Settled` + `ReqCtx.timing.settled_ms = now` | `crates/gw-proxy/src/filters/response.rs` |
| `logging` 入口检测当前 exposure ∈ {`Sent`, `Streaming`}(即未达终态) | `* → Terminated` + `ReqCtx.timing.settled_ms = now` | `crates/gw-proxy/src/filters/logging.rs` |

转移函数集中在 `crates/gw-proxy/src/billing_lifecycle.rs`(新增),所有 filter 通过 `lifecycle::transition_to(...)` 调用,不允许散点写状态。

### 5.2 UsageExtractor(解决 L1)

6 维 × 3 codec 提取矩阵,单一职责模块 `crates/gw-codec/src/usage_extractor.rs`(新增),替换当前散落在 codec 的字段提取。

**提取矩阵**:

| Codec | input | output | cache_read | cache_creation | thinking | multimodal_input |
|---|---|---|---|---|---|---|
| OpenAI Chat Completions | `usage.prompt_tokens` | `usage.completion_tokens` | `usage.prompt_tokens_details.cached_tokens` | — (OpenAI 无写缓存费率分项) | `usage.completion_tokens_details.reasoning_tokens` | — |
| OpenAI Responses API | `usage.input_tokens` | `usage.output_tokens` | `usage.input_tokens_details.cached_tokens` | — | `usage.output_tokens_details.reasoning_tokens` | — |
| Anthropic Messages | `usage.input_tokens` | `usage.output_tokens` | `usage.cache_read_input_tokens` | `usage.cache_creation_input_tokens` | — (Claude extended thinking 已计入 output_tokens) | — |
| Gemini Generate | `usageMetadata.promptTokenCount` | `usageMetadata.candidatesTokenCount` | `usageMetadata.cachedContentTokenCount` | — | `usageMetadata.thoughtsTokenCount` | `usageMetadata.promptTokensDetails[modality∈{IMAGE,VIDEO,AUDIO}]` 求和 |

**接口**:
```rust
pub trait UsageExtractor {
    /// 从 ResponseIR / 单条 stream chunk 中提取 usage 增量。
    /// 返回 (per_dim_values, per_dim_provenance)。
    fn extract(&self, ir: &ResponseIR) -> ExtractedUsage;
    fn extract_chunk(&self, ev: &StreamEventIR) -> Option<ExtractedUsage>;
}

pub struct ExtractedUsage {
    pub values: UsageSnapshot,             // 各维 Some/None
    pub provenance: UsageProvenance,       // 各维 tag
    pub is_terminal: bool,                 // 是否是 final usage event (决定 Streaming → Settled)
}
```

**调用方**:`response_body_filter`(stream 每 chunk + 非 stream EOS),累积到 `ReqCtx.usage` 与 `ReqCtx.usage_provenance`。

**logging.rs:204-207 修复**:6 字段全部从 `ReqCtx.usage` 读,不再写 `None`。

### 5.3 UpstreamCancellationCoordinator(解决 L3 / L7)

监听三个 cancel 源,统一处理:

| 源 | 触发条件 | 探测点 |
|---|---|---|
| 客户端 TCP close / HTTP-2 RST_STREAM | `pingora_proxy::Session::is_body_done()` 异常 / IO error | `response_body_filter` 每 chunk 末尾 |
| 网关 response deadline 超时 | `tokio::time::timeout` 包住 upstream body 读取,超时 `ReqCtx.cfg.response_deadline_ms` | `LlmGateway::response_body_filter` |
| 上游主动 RST / 5xx 中途 | Pingora 给出的 `pingora::Error` | `response_filter` / `response_body_filter` |

任一触发后:
1. `VendorExposureTracker.observe_cancel_trigger(cause)`(记录原因到指标,不改 `VendorExposure` 枚举值 — 状态仍是 `Streaming`)
2. 主动关上游 stream(`session.set_keepalive(None)` + 上游 stream RST,具体 API 在 implementation plan 确定)
3. 进入内部 grace 阶段:`tokio::time::timeout(cancel_grace_ms)` 包住上游 body 继续读 — 有些厂商在 cancel 后仍发一个 final usage frame,必须截下来。grace 期内若 EOS / usage event 到达,正常累积到 `ReqCtx.usage`
4. grace 到 / 上游 EOF → `VendorExposureTracker.transition_to(Terminated)` → 进 `logging` emit

**`cancel_grace_ms` 是关键可调参数**,值过小会漏掉 final usage frame(命中率低),过大会拖慢 cancel 路径。`gw_billing_grace_yielded_final_usage_total{yielded}`(见 §11)是调参信号 — `yielded=true` 占比偏低提示 grace 不够,可据此上调。

**配置**:`config/default.toml` 新增:
```toml
[billing]
cancel_grace_ms = 200             # 上游 cancel 后等待 final usage frame 的窗口 (ms)
response_deadline_ms = 300_000    # 上游响应总超时, 默认 5min, 与 Anthropic / Gemini 上游 timeout 上限对齐
```
均可被 `local.toml` / `GW_BILLING__CANCEL_GRACE_MS` env 覆盖。

### 5.4 InflightLedger(解决 L4 / L5)

**Key 结构**(`crates/gw-ledger/src/inflight.rs` 新增):
```
billing:inflight:<instance_id>:<request_id>:<attempt_index>   HASH
  state              "sent" | "settled" | "terminated"
  ts_sent_ms         i64
  ts_settled_ms      i64    (空表示尚未 Settled/Terminated)
  user_id            string
  vendor_key_id      string
  freeze_amount      u64
  prompt_estimate_tokens u32
EXPIRE 86400  # 24h, 仅安全网, 不参与计费决策
```

#### 5.4.1 instance_id — Redis 持久化的稳定身份(L0 / L1 / L2 三级)

`instance_id` 必须**跨进程重启稳定**,以便重启后的本进程识别上一次崩溃残留的 key。三级解析:

| 级别 | 来源 | 适用 |
|---|---|---|
| L0 | env `GW_INSTANCE_ID` | 运维强绑定 / 容器编排(推荐生产) |
| L1 | in-process `static OnceLock<String>` | 当前进程缓存,启动后第一次解析后写入 |
| L2 | Redis 持久化 `gateway:instance:<hostname>`(无 TTL) | 默认路径:重启后自动 load 同 instance_id |

**启动期解析逻辑**:
```
1. 读 env GW_INSTANCE_ID; 命中即用, 同步覆写 L2 (`SET gateway:instance:<hostname>` 不带 TTL)
2. 否则读 L2 (`GET gateway:instance:<hostname>`); 命中即用
3. 否则生成 UUID v4, `SET gateway:instance:<hostname>` 写入 L2 (无 TTL), 使用
4. 写 L1 OnceLock
5. 同启动一个 heartbeat 周期 task: `SET gateway:instance:<id>:heartbeat <ts_ms> EX 600`, 周期 60s 续期 — 供 Go-side global reaper 判断 dead instance
```

`<hostname>` 来源:Linux `hostname(1)` 系统调用(`HOSTNAME` env 覆盖),k8s StatefulSet 下 pod name 天然稳定;非 StatefulSet 部署需配 L0(`GW_INSTANCE_ID`)避免重调度后 instance_id 漂移。

**效果**:
- 同 replica 重启 ⇒ 同 instance_id(L2 load)— 重启后 reconciler 能识别自己上一次的残留
- 不同 replica 部署 ⇒ 不同 instance_id(不同 hostname → 不同 L2 key)— 互不踩
- 运维强绑定 ⇒ 配 L0,L2 仅作 cache 同步

**生命周期接口**:
```rust
impl InflightLedger {
    pub async fn register(&self, ctx: &ReqCtx) -> Result<()>;       // Sent 转移时调用
    pub async fn mark_settled(&self, ctx: &ReqCtx) -> Result<()>;   // Settled/Terminated 转移时调用
    pub async fn clear(&self, ctx: &ReqCtx) -> Result<()>;          // emit UsageRecord 成功后调用
}
```

> **Note (2026-05-29 follow-up)**: `clear` is **not** called by the proxy. Spec [`2026-05-29-billing-record-durability`](./2026-05-29-billing-record-durability-design.md) §3.2 tightens the contract so `clear` is owned by the ledger and only fires after a confirmed XADD on `usage:records`. The `InflightLedger::clear` API itself is unchanged; only the call site moved.

#### 5.4.2 Trigger 1 — 在线 watchdog(T0 emit-lag 探测,不自愈)

`crates/gw-ledger/src/inflight_reaper.rs` 新增周期 task:
- 周期 `reaper_interval_secs`(默认 30s)
- `SCAN MATCH billing:inflight:<self_instance_id>:*`
- 对每条已写 `ts_settled_ms` 的 key,检查 `now - ts_settled_ms > emit_lag_t0_secs`(默认 60)
- 命中 → 增 `gw_billing_emit_lag_t0_total{request_id, attempt_index}` counter + log `ERROR` 级
- **不**做 fallback emit;保留 key 在 Redis 24h 供事后排查
- 命中 = emit pipeline 卡死(mpsc 堵 / flusher 死 / XADD 永久失败),**T0 incident**

**关键点**:60s 计时从 `ts_settled_ms` 起算,**不**从 `ts_sent_ms` 起算。一个 o1 思考 300s 是合法长流式,不触发;一个 emit 卡 60s 是病态,触发。两件事彻底解耦。

#### 5.4.3 Trigger 2 — 启动期 reconciler(崩溃恢复,唯一 emit fallback 的地方)

`crates/gw-ledger/src/inflight_reconciler.rs` 新增,启动时执行一次(在 L2 instance_id 解析完成后):
1. 加载本进程 `self_instance_id`(§5.4.1)
2. `SCAN MATCH billing:inflight:<self_instance_id>:*`(**只**扫自己 instance_id 的 key)
3. 对每条 emit 一条 `CrashFallback` UsageRecord:
   - `input_tokens = prompt_estimate_tokens`,其他 5 维全 0
   - `vendor_exposure_state = CrashFallback`
   - `usage_provenance.*` = `CrashFallback`
   - `attempt_index` 从 key 解析
4. emit 成功后 DEL key
5. 增 `gw_billing_inflight_recovered_total{exposure_state}` counter

**为什么扫自己 instance_id 而非其他**:
- L2 持久化保证重启后 `self_instance_id` 不变 → 旧残留 key 必然在自己 instance_id 下
- 其他 replica 的 key 由其各自启动 reconciler 处理,本进程不动
- 多 replica 边界清晰,无跨 replica 协调

**Orphan instance handling**(dead / decommissioned replica):
- heartbeat key `gateway:instance:<id>:heartbeat` EX 600s 未续 → instance 视作 dead
- Rust 侧**不**扫 orphan(避免跨 replica 协调复杂度)
- 24h `EXPIRE` 是兜底 — 长寿命 inflight key 通常表示 instance 已死或卡死
- 推荐 Go-side global reaper 周期扫 `billing:inflight:<id>:*`,对 heartbeat 已过期的 `<id>` 做 fallback emit;**Go-side global reaper 不属本 spec 范围**,在 `docs/go-contracts/parts/02-go-as-consumer.md` §2.x 列为 "推荐 Go 侧能力"

### 5.5 UsageRecordEmitter(must-deliver, 与 detailed-log drop-on-outage 严格区分)

| 维度 | usage:records(计费,must-deliver) | usage:detailed(归档,drop-on-outage) |
|---|---|---|
| mpsc full | **同步 XADD bypass channel**(`crates/gw-ledger/src/flusher.rs` 加 fallback path) | 丢 + 计数 |
| XADD 失败 | DLQ `usage:records:dlq`(已存在) | 丢 + 计数 |
| logging filter 路径 | **无条件 emit**,即使 ctx 半残也用 fallback 值 emit | 视字段可用性 |
| Rust 进程崩溃 | **InflightLedger Trigger 2 兜底**(§5.4) | 丢(已接受) |

**fallback emit 规则**(`crates/gw-proxy/src/filters/logging.rs`):
- ctx 缺 `user_id` → 用 `UserId::unknown()` sentinel
- ctx 缺 `vendor_key_id` → 用 `KeyId::unknown()` sentinel
- ctx 缺 `usage` → 全 0 + `provenance.*=Unknown`
- 标 `vendor_exposure_state=Terminated`、`http_status=0`
- 仍 emit。Go settler 看到 `Unknown` provenance + sentinel id 应路由到 `usage:records:dlq` 等价的人工审查 queue(Go 侧规则,本 spec 不展开)

### 5.6 AttemptManifest(解决 L6)

每次路由层切 vendor key 前:
1. 当前 `BillingTransaction.exposure` 强转 `Terminated`(前次部分 output 必须 emit)
2. 调用 `inflight.mark_settled() + emit UsageRecord + inflight.clear()`
3. `attempt_index += 1`,新建 `BillingTransaction`,新 `InflightLedger.register()`
4. 同一 `request_id` 的多条 UsageRecord 由 Go settler 按 `request_id` 聚合做用户层对账

**实施位置**:路由层 retry 切换点(`crates/gw-proxy/src/routing/retry.rs` 或现有等价位置,在 implementation plan 中确认)。

---

## 6. UsageRecord schema 一次性破坏性升级

**遵循用户已确认的"未上线、不保留兼容路径、Schema 改动直接全量切换"原则**。

```rust
pub struct UsageRecord {
    // === 保留字段 ===
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
    pub status: UsageStatus,
    pub error_code: Option<String>,
    pub http_status: u16,
    pub usage: UsageSnapshot,
    pub freeze_amount_micro: u64,
    pub estimate_pricing_version: u64,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub is_stream: bool,

    // === 新增 ===
    pub attempt_index: u32,                       // 0 = 首次,重试递增
    pub vendor_exposure_state: VendorExposureState,
    pub usage_provenance: UsageProvenance,

    // === 移除 ===
    // pub usage_source: UsageSource,             // 被 vendor_exposure_state + usage_provenance 取代
    // pub retry_count: u32,                      // 被 attempt_index 取代(每 attempt 自带 index, 不再累计计数)
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VendorExposureState {
    Settled,           // 拿到 final usage event/payload
    Terminated,        // 中断,但已发出过请求,有 partial 或 input-only usage
    CrashFallback,     // InflightLedger reconciler 兜底, usage 全估算
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceTag {
    UpstreamFinal,     // 厂商发了 terminal usage event/payload
    UpstreamPartial,   // 流中解析的部分值,但没等到 final
    Estimated,         // 退化到 tokenizer 计数
    CrashFallback,     // InflightLedger reconciler 写入
    Unknown,           // 上游根本没产出 / ctx 半残
}

#[derive(Serialize, Deserialize)]
pub struct UsageProvenance {
    pub input: ProvenanceTag,
    pub output: ProvenanceTag,
    pub cache_read: ProvenanceTag,
    pub cache_creation: ProvenanceTag,
    pub thinking: ProvenanceTag,
    pub multimodal_input: ProvenanceTag,
}
```

**Go-contracts/parts/02 §2.1 同 PR 更新**:见 §8.

---

## 7. 边界场景覆盖矩阵

| # | 场景 | exposure | provenance(关键维) | 出账准确度 |
|---|---|---|---|---|
| 1 | client 断流中途 | `Terminated` | `output=UpstreamPartial` | 真实 partial |
| 2 | client 断在第一 chunk 前 | `Terminated` | `input=Estimated, output=Unknown` | input 用 prompt estimate |
| 3 | HTTP/2 RST_STREAM | `Terminated` | 同 #1 | 同 #1 |
| 4 | 网关 deadline 超时 | `Terminated` | 同 #1 | 同 #1 |
| 5 | 上游 SSE error event 中途(含 content) | `Settled` | `output=UpstreamPartial`(error 视作 final) | 真实 partial |
| 6 | 上游 5xx 中途 | `Terminated` | `output=UpstreamPartial` | 真实 partial |
| 7 | cross-protocol re-render 失败 | `Settled` | `*=UpstreamFinal`(read 与 render 解耦) | 完整真实 |
| 8 | non-stream JSON 解析失败 | `Terminated` | 全 `Estimated` | tokenizer 估算兜底 |
| 9 | 上游不发 usage event(stream 完结) | `Terminated` | `input=Estimated/Final, output=Estimated` | output 用 tokenizer 估算 |
| 10 | client 断但 usage 已 parse | `Settled` | `*=UpstreamFinal` | 完整真实(grace 时序保证) |
| 11 | cache_read / cache_creation / thinking / multimodal_input | `Settled` | 各维 `UpstreamFinal` | 完整真实(UsageExtractor 全填) |
| 12 | freeze > actual / freeze < actual | — | — | Rust release; Go settler 看 UsageRecord 做 delta DEDUCT |
| 13 | Rust 崩溃 between Sent and emit | `CrashFallback` | 全 `CrashFallback` | InflightLedger Trigger 2 emit `input=estimate, output=0` |
| 14 | 多 key 重试 | 每 attempt 一条 record | per-attempt 独立 | 每 attempt 独立,Go 按 `request_id` 聚合 |
| 15 | tool-call 多轮 | 每轮独立 request | per-request 独立 | 各轮独立(本 spec 模型天然支持) |

---

## 8. Go-contracts 影响(parts/02 §2.1 同 PR 更新)

### 8.1 新增字段

`usage:records` payload 加 3 个新字段:`attempt_index` / `vendor_exposure_state` / `usage_provenance`,移除 `usage_source` / `retry_count`(语义被替代)。

### 8.2 去重 contract(新增)

**Go settler 按 `(request_id, attempt_index)` 主键去重,同主键多条按 provenance 优先级择高:**

```
优先级(从高到低):
  UpstreamFinal > UpstreamPartial > Estimated > CrashFallback > Unknown
```

**算法**:
1. Go settler 维护 `(request_id, attempt_index) → 已 settle 的 provenance level` 的短期 dedup 表(TTL 24h 与 InflightLedger EXPIRE 对齐)
2. 收到一条 record:
   - 主键不存在 → 直接 settle,记入 dedup 表
   - 主键存在 + 新 record 的 provenance 更高 → 应用 delta(新 - 旧)更新 balance,dedup 表更新到新 level
   - 主键存在 + 新 record 的 provenance 更低或等同 → 丢弃(已被更准确的记录覆盖)
3. 崩溃恢复的 `CrashFallback` record 永远会被同主键的真实 emit 覆盖,无需 Rust 侧做 correction

**实施位置**:`docs/go-contracts/parts/02-go-as-consumer.md` §2.1 重写,新增 §2.1.x "去重规则" 子段。

### 8.3 Sync rule 触发

本 spec 上线 PR 内 必同步更新:
- `docs/go-contracts/parts/02-go-as-consumer.md` §2.1 — usage:records schema + 去重规则
- `docs/go-contracts/main.md` §3.3 表 — 如有 stream consumer group 命名变更(本 spec 不变)
- `CLAUDE.md` §Sync rule 已有 go-contracts 触发行,无需改

---

## 9. 与 v2 cleanup sub-spec 的吸纳关系

**不另起独立 sub-spec**,按职责吸纳:

| 子系统 | 归属 |
|---|---|
| §5.2 UsageExtractor | **S-codec 扩**(6 维 × 3 codec 提取矩阵属 codec 职责) |
| §5.5 UsageRecordEmitter must-deliver | **S-ledger 扩** |
| §5.6 AttemptManifest | **S-ledger 扩** |
| §6 UsageRecord schema | **S-ledger 扩**(go-contracts 同 PR 协调) |
| §5.4 InflightLedger | **S-ledger 扩** |
| §5.1 VendorExposureTracker | **S-proxy-core 扩** |
| §5.3 UpstreamCancellationCoordinator | **S-proxy-core 扩**(吸纳现有 response deadline 项) |

**跨 sub-spec 协调点**:`ReqCtx` 上新增字段(`vendor_exposure`、`timing.sent_ms`、`timing.settled_ms`、`attempt_index`、`usage_provenance`)— 在 W2 一次性加入,后续 wave 复用。

**实施 ownership**:
- S-ledger 部分 PR 与 S-proxy-core 部分 PR 可并行,但 W4 schema 切换必须等两者都到位

---

## 10. 实施波次

| Wave | 内容 | 破坏性 | 单独可发布 |
|---|---|---|---|
| **W1**(止血) | UsageExtractor 6 维填全(§5.2,L1 修复)+ UsageSource 临时修正(L2 用现 enum 表达 `Upstream` when input > 0 even if output == 0)+ 新增 metrics | 否 | 是,**当天上** |
| **W2**(lifecycle 骨架) | VendorExposureTracker(§5.1)+ ReqCtx 新字段 + InflightLedger register/clear + **instance_id L0/L1/L2 三级解析 + heartbeat task**(§5.4.1)+ UsageRecordEmitter must-deliver(§5.5)(此波 reaper / reconciler 尚未启用) | 否 | 是 |
| **W3**(取消传播) | UpstreamCancellationCoordinator(§5.3,grace 内部化)+ response deadline 集成(吸纳 S-proxy-core 项) | 否 | 是 |
| **W4**(崩溃恢复 + schema) | InflightLedger Trigger 1 (T0 watchdog,§5.4.2) + Trigger 2 (启动 reconciler,§5.4.3)+ UsageRecord schema 升级(§6,破坏性)+ AttemptManifest(§5.6)+ go-contracts/parts/02 同 PR 改 | **是**(Go PR 同步) | 是 |

每波结束都可单独发布、监控、回滚。**W1 是当天止血**,L1+L2 漏点立刻闭合。

---

## 11. Metrics

新增 `gw_billing_*` 命名空间:

- `gw_billing_emit_total{exposure_state, provenance_output}` — counter,每条 UsageRecord emit 时增
- `gw_billing_emit_lag_t0_total{request_id, attempt_index}` — counter,**任何非 0 都是 on-call**(§5.4.2)
- `gw_billing_inflight_recovered_total{exposure_state}` — counter,启动 reconciler 兜底 emit 计数
- `gw_billing_instance_heartbeat_renewals_total` — counter,heartbeat 续期成功次数(§5.4.1);停滞表示 Redis 写不进
- `gw_billing_upstream_cancellation_total{cause}` — counter,cause ∈ `{client_close, deadline, upstream_err, route_abort}`
- `gw_billing_attempts_per_request` — histogram,验证多 key 重试的实际触发分布
- `gw_billing_grace_yielded_final_usage_total{yielded}` — counter,验证 `cancel_grace_ms` 默认 200 是否合理(`yielded=true` 表示 grace 期内截到了 final usage frame)
- `gw_billing_mpsc_bypass_total` — counter,must-deliver 走同步 XADD 的频次(mpsc 容量调优信号)
- `gw_billing_provenance_total{dimension, tag}` — counter,每维 × 每来源,定位"哪个 codec 的哪个维度估算率偏高"

**告警**:
- `gw_billing_emit_lag_t0_total > 0` 任意一秒 → page on-call,严重等级 T0
- `gw_billing_inflight_recovered_total` 1 分钟增量 > 10 → warning(异常崩溃率)
- `gw_billing_mpsc_bypass_total / gw_billing_emit_total > 1%` → warning(mpsc 容量不足)

---

## 12. Deliberate behaviours(本 spec 引入,需同步进 architecture.md §Deliberate behaviours)

1. **客户端断流不取消计费**:`ClientDelivery::Disconnected` 只触发上游 cancel,不阻止 UsageRecord emit。理由:厂商已扣费,我方有义务出账。
2. **崩溃恢复 emit 估算**:`CrashFallback` UsageRecord 的 `output_tokens=0`、`input_tokens=prompt_estimate`,**是已知低估**。理由:崩溃窗内 partial usage 在 RAM,不在 Redis;唯一的兜底是 prompt estimate。生产应保证 Rust 进程 MTBF >> 单请求时长,使此路径罕见。
3. **must-deliver vs drop-on-outage 严格区分**:`usage:records` mpsc 满走同步 XADD bypass(可能短暂阻塞 logging filter);`usage:detailed` mpsc 满直接丢。两者**故意**采用不同策略,反映"账务必送达 / 归档可丢"的不变量差异。
4. **T0 watchdog 不自愈**:60s emit-lag 命中后只告警,不自动 emit fallback。理由:自愈会与最终成功的真实 emit 双计费;让 on-call 介入查根因更合适,真账已被 InflightLedger 保留 24h。
5. **去重交给 Go**:Rust 侧可能 emit 同一 (request_id, attempt_index) 的多条 record(崩溃恢复 + 真 emit 并存),Rust 不做幂等,由 Go settler 按 §8.2 规则去重。理由:Rust 端做 idempotency 需要跨进程协调,代价 > 收益。

---

## 13. Known gaps / future work

- **Tokenizer 准确度**:`prompt_estimate_tokens` 当前可能与厂商真实计数有 ±5% 偏差(各家 tokenizer 实现差异)。崩溃恢复 fallback 的准确度受此限制。改进方向:每个 vendor 维护各自 tokenizer 实现,但工程量大,本 spec 不展开。
- **partial usage 持久化**:崩溃前流中已 parse 的 partial usage 没写 Redis,崩溃后丢失。可选优化:Settled/Terminated 转移时把 `ReqCtx.usage` snapshot 写进 InflightLedger HASH,reconciler 优先用 snapshot 而非 estimate。代价是每请求多一次 HSET,本 spec 不引入,作为后续优化。
- **Go settler 去重表 TTL**:24h 与 InflightLedger EXPIRE 对齐,但若 Go settler 重启,dedup 表丢失,极端情况下崩溃恢复的 CrashFallback 可能被当成首次 settle。改进:Go settler 持久化 dedup 表(自决,本 spec 不规约)。
- ~~emit fast path 持久化语义不充分(proxy 侧 `inflight.clear` 与异步 XADD 竞争,flusher panic 可能静默丢记录)~~ **closed-by** [`2026-05-29-billing-record-durability`](./2026-05-29-billing-record-durability-design.md)(proxy 侧 `inflight.clear` 移除;ledger 在 XADD 成功后独占 DEL;flusher panic 经 `catch_unwind` 隔离;`Ledger::emit` 的 Closed 分支回退 `sync_xadd`)。
- **hostname 冲突场景**:非 k8s / 无 StatefulSet 部署时,多 replica 可能拥有相同 hostname(如直接跑在多台 VM 上的 `localhost` 默认主机名)。此时 L2 解析会让多 replica 共享 instance_id,inflight key 互踩。**缓解**:生产部署强制设 `GW_INSTANCE_ID`(L0),启动期检测到 hostname 在 L2 已存活且 heartbeat 仍新鲜 → fail-fast 报错并要求显式 instance_id 配置(implementation 阶段确定具体检测策略)。
- **Orphan instance 兜底**:Rust 不扫其他 instance_id 的残留 key,长期 dead replica 的 24h EXPIRE 之内的 inflight key 不会被自动恢复;依赖 Go-side global reaper(`docs/go-contracts/parts/02` 列为推荐能力)。该 reaper 不属本 spec 范围。
- **HTTP/3**:当前 Pingora 0.8 仅 HTTP/1.1 + HTTP/2,cancel 探测在 H/3 上是否同样工作未验证。
- **WebSocket tunnel**:tunnel 路径下 cancel 传播的具体语义(WS frame cancel vs HTTP cancel)需在 tunnel 子系统 spec 中单独处理。

---

## 14. 自检 checklist

PR 上线前确认:
- [ ] `logging.rs:204-207` 三字段不再写死 `None`
- [ ] `cargo test -p gw-codec usage_extractor::` 全过
- [ ] `cargo test -p gw-ledger inflight::` 全过
- [ ] `gw_billing_emit_lag_t0_total` 在 e2e soak 测试 60s 窗口内保持 0
- [ ] `bash scripts/check-invariants.sh` 通过(`no_cost_micro` / `no_billing_crate` 不变)
- [ ] `docs/architecture.md` §Deliberate behaviours 新增 §12 五条
- [ ] `docs/go-contracts/parts/02-go-as-consumer.md` §2.1 同步更新去重规则
- [ ] `CLAUDE.md` 计费/Observability 段落如有 v1 表述,同步更新
- [ ] v2 cleanup 的 S-ledger / S-proxy-core sub-spec 索引页(`2026-05-23-v2-cleanup-overview-design.md`)标注本 spec 已吸纳的项

---

## 15. 实施入口

> Plan via `superpowers:writing-plans` → `docs/superpowers/plans/2026-05-24-billing-robustness.md`(待生成)。
> 每个 Wave 独立 plan 章节,Wave 内任务按 TDD 步骤拆解。
