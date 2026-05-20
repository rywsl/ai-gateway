# v2 整合设计 — 33 份 5-19 spec 冲突收口（v2026-05-20）

- **日期**：2026-05-20
- **状态**：Draft（待用户 review）
- **作者**：协作整理（Claude + 用户）
- **范围**：收口 `docs/superpowers/specs/2026-05-19-*.md` 33 份 spec 之间的显式 + 隐性冲突；固化跨 spec 不变量；给出 7 个细粒度实施 phase 的依赖图、进入 / 退出条件、长期防御线 lint。
- **不在范围**：
  - 修改 `2026-05-18-llm-gateway-design.md` 内容（§2 表 D 仅指出失配点，不更新主设计）；
  - 修改现有 3 份 plan（`business-split-refactor` / `v1-warm-path-fix` / `v1.1-residual-issues`）；
  - 任何代码改动；
  - 任何 phase 的实施 plan（本 spec 通过后通过 writing-plans 进入 Phase 1）。

> 项目当前未上线。本 spec 默认**破坏性改动允许**：直接删除被取代的字段 / API / stream，**不留** `#[deprecated]` 桩、双写期、re-export 兼容、灰度方案。

---

## §1 范围

### §1.1 整合背景

`docs/superpowers/specs/` 下 33 份 2026-05-19 spec 是过去一周对 5-18 主设计的全面增量。多份已自带 "撤回 vX" 标记（如 A-T0 撤回 adapter-layer-design ADR-A2、codec S1 v2 撤回 IR runtime path、business-split-refactor v2 撤回 gw-billing）。本 spec：

1. 把所有"撤回"集中到 §2 表 A 一刀切清单；
2. 收口我在 review 时发现的 6 条隐性冲突（§2 表 B）；
3. 把跨 spec 命名空间约定锁死成 8 条不变量（§3）；
4. 重画 phase 依赖图 7 节点（§4）；
5. 给后续每份 plan 提供长期防御线 lint（§5.2）。

### §1.2 与 5-18 / 现有 plan 的关系

- **5-18 不动**：5-18 ADR-019（IR runtime path）、ADR-008（breaker per-key 单维）、ADR-013（四协议）等已被本 v2 周期 spec 取代，但 5-18 文档不更新——本 spec §2 表 D 仅记录失配点供历史 review。
- **现有 plan 不动**：`business-split-refactor.md` v2 已自我修订过一次，但文档内部仍有 v1 残留段（line 188-345）。本 spec 不重写该 plan，仅在表 B 注明"plan 需后续整理"，待 Phase 1 落地时一并清理。

---

## §2 冲突解决账本

### §2.1 表 A — 一刀切清单（13 项，显式撤回）

未上线项目无兼容窗口：以下 13 项在被取代的 spec / plan 落地时**直接删除**，不留 deprecated 桩、不留 re-export。

| # | 被删项 | 取代方案 | 依据 |
|---|---|---|---|
| A1 | `gw-routing::retry.rs` 全文件 | `gw-routing::failover.rs`（FailoverState + classify_for_failover） | R-E ADR-RE-4 |
| A2 | `gw-routing::breaker.rs` 全文件 | `gw-health::KeyHealthStore` + `gw-routing::in_flight::InFlightStore` 拆分 | K-A ADR-KA-1 / ADR-KA-8 |
| A3 | `gw-ledger::FreezeGuard::settle(actual)` API | `FreezeGuard::release()`；不写 deducted | C-γ ADR-Cγ-2 |
| A4 | `BillingRecord` 结构体 + 字段 `cost_micro` | `UsageRecord` + `UsageSnapshot` 6 维 + `MarkupChainSnapshot` | C-α ADR-Cα-1 / ADR-Cα-2 |
| A5 | Redis Stream `billing:records` | `usage:records` | C-α ADR-Cα-3 |
| A6 | `gw-ledger::flusher` 的 `INCRBY counter:user:{uid}:balance_micro -cost` 路径 | flusher 仅 `XADD usage:records`；Go 在 settle 时写 deducted | C-γ ADR-Cγ-6 |
| A7 | `gw-billing` crate（在 business-split-refactor.md Step 3 已撤销，未来 spec/plan 不允许复活） | 公式整体归 Go；Rust 仅 UsageRecord 记录 | business-split-refactor.md v2 修订 + 桶 6 五份 spec |
| A8 | `UpstreamAdapter::patch_body` + `BodyPatch::Splice` / `HeaderOnly` 枚举 | `patch_request(&mut WireAnyRequest)` + `patch_response(&mut WireAnyResponse)` | A-T0 ADR-AT0-1 |
| A9 | `gw-codec-macros::{IrConvert, IrStreamConvert, IrErrorConvert}` 三 derive 概念 | 单一顶层 `wire_convert_matrix!` 宏 + 编译期 schema | S1 v2 §ADR-S1-v2-2 |
| A10 | `gw-core::ir::*` 公开构造函数 + Default impl + Serialize/Deserialize | `#[ir_schema]` 标注 + `#[non_exhaustive]` 阻止实例化；模块改名 `gw-core::schema` | S1 v2 §ADR-S1-v2-1 |
| A11 | `RetryConfig::mode: RetryMode { SameKey / CrossKey / Mixed }` 三态 | 撤销开关；列表语义由 R-AB SelectorAny + R-E FailoverConfig 统一表达 | R-E ADR-RE-4 |
| A12 | `DeductionState::deducted_micro.fetch_add(actual)` Rust 写权 | Rust read-only via `events:counter`；Go 唯一 writer | C-γ ADR-Cγ-1 |
| A13 | `MergedPermissions::markup_product: f32` 单标量字段 | `MergedPermissions::markup_chain: MarkupChainSnapshot`（5 字段） | C-α ADR-Cα-6 |

### §2.2 表 B — 隐性冲突主决议（6 条）

| # | 冲突点 | 决议 | 依据 |
|---|---|---|---|
| B1 | task-worker-design v3.2 ADR-T1 依赖图仍列 `gw-billing` 业务 crate | **`gw-billing` 不存在**；worker 仅采集 `duration_seconds` / `task_type` 字段写 UsageRecord；video / 阶梯 / markup 公式归 Go | gap-inventory 桶 6 v2 范围调整 + business-split-refactor.md v2 + 表 A A7 |
| B2 | task-worker-design ADR-T8 settle.lua 写 `HINCRBY counter:user:<uid>:balance:deducted +actual` | task settle 也 **release-only**：worker 仅 `HINCRBY task_frozen_micro -frozen` + `HSET entity:task status=终态` + `ZREM tasks:leases` + `XACK tasks:queue`；不写 deducted；不算 actual cost | C-γ ADR-Cγ-1（Go 是 deducted 唯一 writer）+ 表 A A12 |
| B3 | task-worker-design ADR-T5 task 计费写 `billing:records` stream | 改 `usage:records`；payload 加 `is_task=true / task_id / duration_seconds / task_type` 字段 | C-α ADR-Cα-3 stream 改名 |
| B4 | business-split-refactor.md plan 内部仍写 `UpstreamAdapter::patch_body` 与 `BodyPatch` | trait 形态以 **A-T0 v2** 为准：4 钩子（`rewrite_auth` / `rewrite_transport` / `patch_request(&mut WireAnyRequest)` / `patch_response(&mut WireAnyResponse)`）+ `transport_profile()`；plan 在 Phase 1 同步修正 | A-T0 ADR-AT0-1 + 表 A A8 |
| B5 | `UsageSnapshot` 类型同时被 codec S4 与 C-α 引用，权威定义位置未指定 | 单一定义于 **`gw-core::usage::UsageSnapshot`**（POD 6 字段）；codec S4 `extract_usage_any` 返回它、C-α `UsageRecord.usage` 嵌入它、worker settle 链路同款 | 表 C C-7 共享类型位置 |
| B6 | C-γ 引入 `events:counter` stream 但 5-18 ADR-013 只规划了 4 个持久化协议 | **5-18 ADR-013 扩展为 5 协议**：新增 "Counter Event"（以 `value_after` 覆盖、独立 Stream key `events:counter`、与 `events:entity` 分流） | C-γ ADR-Cγ-4 |

### §2.3 表 B 续 — 机械收口（7 条）

非主决议，但在各 phase plan 中需机械落实：

| # | 项 | 落点 |
|---|---|---|
| Bm1 | `gw-auth::MergedPermissions.markup_product: f32` → `markup_chain: MarkupChainSnapshot` | Phase 5 |
| Bm2 | `gw-routing::PickCtx` 加 `api_format: ApiFormat` 字段；strategy::pick 过滤候选时调 `KeyHealthStore::is_available(key, fmt)` | Phase 4 |
| Bm3 | `gw-health` 作为新业务 crate 登记到 C-1 依赖图与 CLAUDE.md crate layering rules | Phase 1 |
| Bm4 | K-B `ErrorOutcome::rate_limit_info: Option<RateLimitInfo>` 字段与 RL-Core 同 PR 实施 | Phase 3（K-B 落地时随手加；Phase 4 RL-Core 实际消费） |
| Bm5 | `K-A ApiFormat = ProtocolKind` 在 v1（不细化为 openai_chat / openai_responses / openai_cli 等子桶） | Phase 1（K-A 落地时锁定） |
| Bm6 | `business-split-refactor.md` plan 内部 line 188-345 v1 残留段：Phase 1 落地时一并清理（连同 patch_body / settle(actual) / gw-billing 引用） | Phase 1 |
| Bm7 | `gw-codec` 同协议 passthrough 路径（`Cow::Borrowed`）必须由 codec S1 v2 的 `Mutations::is_empty()` 守护；adapter `patch_request` 通过 helper 自动 set bit，不可直接改 wire 字段绕过 | Phase 2 |

### §2.4 表 D — 5-18 ↔ 5-19 失配点登记（5 项，不修 5-18）

| # | 5-18 条目 | 失配点 | 5-19 取代者 |
|---|---|---|---|
| D1 | ADR-008 breaker per-key 单维 sliding window | 已被多维 (KeyId, ApiFormat) + HTTP 码分级冷却替换 | K-A `gw-health` |
| D2 | ADR-013 持久化四协议（Entity / Counter / Append log / Lock） | 升级为五协议（+ Counter Event）；events:counter 独立 stream | C-γ ADR-Cγ-4 |
| D3 | ADR-019 协议 IR + ProtocolCodec（IR 是 runtime 数据通路） | IR 降级为编译期 schema；runtime 走 N² wire→wire 直转 | codec S1 v2 |
| D4 | ADR-023 五级权限合并产出 `markup_product: f32` 单标量 | 改为 `MarkupChainSnapshot` 5 字段（user_group / api_key / vendor / model / product） | C-α ADR-Cα-6 |
| D5 | ADR-004 三层余额 Rust 写 deducted | Rust 只写 frozen；Go 是 base/deducted 唯一 writer | C-γ ADR-Cγ-1 |

> 5-18 不更新。Phase 7 收口时在 CLAUDE.md 加注"5-18 主设计的 ADR-008/013/019/023/004 已被 v2 周期取代，权威以本 spec §3 + 各 5-19 spec 为准"。

---

## §3 全局不变量清单

### C-0 破坏性改动模式（项目未上线）

- 任何 schema / API / stream key / trait / 字段改名：同 PR 一刀切，**不留** `#[deprecated]` 桩、re-export、双写期；
- 5-18 与 5-19 间的冲突直接以 5-19 为新权威；5-18 仅作历史背景；
- 表 A 13 项一次性废止；
- 删除被撤销的 spec / plan / 代码而非 "deprecated"。

### C-1 Crate 分层与依赖

```
支撑层（不知业务）：
  gw-core         ◄── 全部
  gw-codec / gw-codec-macros
  gw-store        （Entity / Counter / Append log / Lock / Counter Event 5 协议；不持业务 namespace）
  gw-auth         （含 ratelimit、含 MergedPermissions.markup_chain）
  gw-ledger       （含 FreezeGuard trait + RequestFreeze + TaskFreeze）

业务层（一业务一 crate）：
  gw-adapter      （UpstreamAdapter v2 + UpstreamErrorAnalyzer + OAuthCache）
  gw-health       （KeyHealthStore）
  gw-routing      （含 in_flight + 8 种 Strategy；Strategy::pick 取 &KeyHealthStore，Phase 1c 由支撑层升入业务层 [^routing-tier]）
  gw-task         （DTO + Redis schema helper + lua 字符串；纯 library）

执行体层（互不依赖）：
  gw-proxy        ◄── gw-gateway binary
  gw-task-runtime ◄── gw-task-worker binary

禁止边（CI grep 守）：
  - 支撑层 → 业务/执行体 crate（任意方向）
  - gw-proxy ↔ gw-task-runtime 双向禁
  - 支撑层引入业务 namespace（task::* / billing::* / health::* / adapter::vendor）
  - gw-billing crate 任何形式复活
```

[^routing-tier]: **Layer reclassification** (Phase 1c, commit `8032b08`): `gw-routing` was originally listed as 支撑层 in this section but its `Strategy::pick` signature took `&KeyHealthStore` from the new business-layer `gw-health`, creating a structural 支撑→业务 edge that the C-1 forbidden-edges rule rejects. Two solutions were considered: (a) reverse-inversion via a trait in `gw-core` (`trait KeyAvailability`) so `gw-routing` keeps its supporting status, or (b) reclassify `gw-routing` to business-layer. Option (b) was chosen because §3 C-7 already designates `gw-health::KeyHealthStore` ↔ `gw-routing` selection as an explicit coupling point — extra indirection would add no value. Future work that needs a routing crate without health coupling can revisit (a).

### C-2 错误码

**新增（`gw-core::error::codes`）**：

```
GW_ROUTING_NO_FALLBACK_LEFT          503    R-E
GW_ROUTING_FAILOVER_TIMEOUT          504    R-E
GW_ROUTING_STREAM_NOT_RETRIABLE      502    R-E
GW_ROUTING_NO_AVAILABLE_CHANNEL      503    R-AB（无候选）
GW_QUOTA_RPM_KEY_EXCEEDED            internal-only（detailed_log root cause；不外泄）
```

**新增（`gw-task::codes`）**：

```
GW_TASK_NOT_FOUND                    404
GW_TASK_ALREADY_TERMINAL             409
GW_TASK_PROVIDER_UNAVAILABLE         503
GW_TASK_QUOTA_EXCEEDED               429
```

**不变（5-18 已有，列出避免误改）**：`GW_AUTH_INVALID_APIKEY` / `GW_QUOTA_RPM_EXCEEDED` / `GW_QUOTA_BALANCE_ZERO` / `GW_UPSTREAM_OVERLOADED` / `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` / `GW_SHORT_CIRCUIT`（内部 marker）。

### C-3 Stream 名

```
被删（一刀切）：
  billing:records

新（Rust XADD，Go consume）：
  usage:records                    XADD UsageRecord payload
  usage:records:dlq                C-δ DLQ
  tasks:queue                      task 提交（gateway XADD，worker consume）
  tasks:cancel                     task 取消（gateway XADD，worker fan-out consume）
  alerts                           告警

新（Go XADD，Rust read）：
  settled_billing                  Go 算 cost 完成后写；Rust 不读
  events:counter                   协议 5；Go 写 base/deducted 后 XADD；Rust consume

不变：
  events:entity                    协议 4；与 events:counter 分流
```

### C-4 Entity / Counter / ZSet / Lock namespace

**Entity**：

```
不变（5-18 已有）：
  entity:vendor:<id>
  entity:vendor_key:<id>             Go writer
  entity:api_key:<id>
  entity:user:<id>
  entity:model_group:<id>
  entity:pricing:<id>                Go writer
  entity:user_group:<id>             Go writer

新：
  entity:estimate_pricing:<id>       C-β；Go writer；Rust read-only
  entity:task:<id>                   gateway HSET 初始，worker HSET 更新；TTL=1d
  entity:worker:<id>                 worker writer；observability only，不参与路由
```

**Counter**：

```
counter:user:<uid>:base_micro              Go writer（Rust read via events:counter）
counter:user:<uid>:deducted_micro          Go writer（Rust read via events:counter）
counter:user:<uid>:frozen_micro            Rust writer（HINCRBY +est on freeze，-est on release）
counter:user:<uid>:task_frozen_micro       Rust writer（gateway +est on submit；worker -frozen on settle）
counter:tasks:running:total
counter:tasks:running:vendor:<vid>         Rust writer
counter:tasks:running:user:<uid>           Rust writer
counter:tasks:running:key:<kid>            Rust writer
counter:task:<task_id>:progress            可选；worker 写
```

**Sorted Set**：

```
tasks:leases                                ZADD by worker；ZRANGEBYSCORE by watchdog
```

**Lock**：

```
lock:tasks:watchdog                          worker leader election（SETNX TTL=10s）
```

### C-5 Schema 版本

```
entity:task.schema_version = 1
  增字段视为兼容；改语义必须 bump version。
  注：项目未上线，v1 无兼容窗口；schema_version 字段保留是为后续上线后准备。

entity:estimate_pricing.version
  Rust freeze 时把 version 快照写入 UsageRecord.estimate_pricing_version。
  Go 算 cost 时记录 skew 到 metric gw_billing_pricing_version_skew_total。

UsageRecord (usage:records 的 payload) schema
  v1 字段集 = C-α §ADR-Cα-1 锁定。增字段兼容，删字段必须新 stream key。
```

### C-6 Metric 命名空间

```
gw_routing_*        candidate / in_flight / scoring                       gw-routing + gw-proxy
gw_failover_*       R-E 独立                                              gw-routing
gw_health_*         circuit / cooldown / record（K-A 唯一）              gw-health
gw_ratelimit_*      user / key / kind_detected / adaptive                 gw-auth
gw_balance_*        frozen / available / event_lag / sync_pending         gw-store + gw-ledger
gw_codec_*          conversion / direction / multimodal / kiro            gw-codec
gw_freeze_*         leak_recovered_total                                  gw-ledger
gw_short_circuit_*  早期短路统计                                          gw-proxy
gw_kms_*            KEK 轮换                                              gw-gateway
gw_adapter_*        adapter 失败 / OAuth refresh                          gw-adapter
gw_task_gw_*        gateway 侧 task 指标                                  gw-proxy
gw_task_*           worker 侧 task 指标（inflight / lease_recovered…）    gw-task-runtime

规则：
- uid 高基数字段不打 label per-uid；走 bucketed（vendor / user_group / api_key 三选一）。
- 被删的 gw_breaker_*（旧 breaker）系列在 Phase 1 一并删除，不保留。
```

### C-7 共享类型权威位置

```
gw-core::usage::UsageSnapshot          POD 6 字段；codec S4 输出 + C-α 嵌入 + worker settle 链路
gw-core::ids::*                        KeyId / UserId / VendorId / ApiKeyId / TaskId
gw-core::protocol::ProtocolKind        ApiFormat = ProtocolKind（v1 alias）
gw-core::error::codes                  跨业务通用 GW_* 错误码
gw-core::schema::*                     codec S1 v2 编译期 schema（非 runtime 实例化）
gw-core::error::ErrorIR                跨 crate 错误传递；含 clone_static()

gw-task::dto::*                        TaskStatus / TaskKind / TaskRecord
gw-task::codes                         GW_TASK_*
gw-task::contract::*                   submit / load / watch / request_cancel / claim / settle helper
gw-task::scripts                       SUBMIT_LUA / SETTLE_LUA / RECOVER_LEASE_LUA 字符串常量

gw-adapter::trait_def::UpstreamAdapter v2   4 钩子 + transport_profile()
gw-adapter::ctx::AdapterCtx                 HeadersMut / UrlMut trait object（runtime-agnostic）
gw-adapter::oauth::OAuthCache               K-C；gw-proxy + gw-task-runtime 共享
gw-adapter::error_analyzer::UpstreamErrorAnalyzer + ErrorOutcome   K-B

gw-health::KeyHealthStore                   K-A；gw-routing 选 key 调 is_available
gw-health::CooldownReason                   16 个 reason（u16 编码）

gw-ledger::freeze::FreezeGuard trait        release-only；FreezeKind { Request, Task }
gw-ledger::freeze::RequestFreeze            请求级 RAII 实现
gw-ledger::freeze::TaskFreeze               task 级显式 lua release 实现

gw-auth::ratelimit::{RateLimitKind, RateLimitInfo}    RL-Core
gw-auth::merge::MarkupChainSnapshot                   5 字段；写入 UsageRecord
```

### C-8 部署不变量（ADR-T16 升级为全局）

- gateway 配置文件 + 启动参数 + 代码 **无** worker host/port；
- worker 配置文件 + 启动参数 + 代码 **无** gateway host/port；
- 唯一耦合面：同一 Redis namespace + KMS KEK + （可选）同一 PgSQL；
- 任意拓扑组合（1×1 / 1×N / N×1 / N×M / 跨地域）必须工作；任意一端先起或单独空跑稳定；
- `entity:worker:<id>` 仅 observability，不参与业务路由；
- 任意一端水平伸缩 / 滚动升级，对端零配置变更、零感知；
- CI lint：`grep` gateway / worker 配置不含对端地址（§5.2 `no_worker_addr_config` / `no_gateway_addr_config`）。

---

## §4 实施 phase

### §4.1 Phase 依赖图

```
Phase 1  脚手架（无业务）
  - gw-adapter trait v2 + Noop + Registry
  - gw-health crate（KeyHealthStore + SlidingWindow）
  - gw-ledger FreezeGuard trait + RequestFreeze（release-only）+ TaskFreeze placeholder
  - codec S1 v2：gw-core::schema + Mutations + wire_convert_matrix! 宏框架 + WireAny enum
  - gw-core::usage::UsageSnapshot 类型定义
  - 删除：gw-routing::retry.rs / gw-routing::breaker.rs / gw-billing 残留 / 旧 BillingRecord.cost_micro / FreezeGuard::settle(actual) / IrConvert 三 derive / business-split-refactor.md plan 内 v1 残留
  - CLAUDE.md：crate layering rules + Billing boundary + Hard deployment guarantee 三条
      │
      ├──► Phase 2  Codec 矩阵（与 Phase 3 并行）
      │     - S2 wire 多模态注解（image / audio / file / tool_use / tool_result / thinking）
      │     - S3 SSE 多事件 + 跨 chunk 累积状态机
      │     - S4 响应 N² + 跨协议 + extract_usage_any
      │     - 9 对 wire 直转 roundtrip 测试
      │     - 撤销 NONSTREAM 400 短路
      │
      └──► Phase 3  Adapter vendor + KeyHealth 业务化（与 Phase 2 并行）
            - A-T1 claude_code（OAuth bearer + system 注入 + beta header）
            - A-T2 gemini_cli（OAuth + quota header）
            - A-T3 vertex_ai（SA JWT 签名 + URL 重写）
            - A-T4 antigravity（URL 可用性探测 + 签名缓存）
            - A-T5 codex（OpenAI Responses 形状 + image_transform + SSE event 改写）
            - K-B vendor error_analyzer + ErrorOutcome（含 rate_limit_info 字段）
            - K-C OAuthCache + 懒刷新（CAS lock）
                  │
                  └──► Phase 4  Routing & RateLimit
                        - R-AB multi-score（CandidateFilter + 8 ScoringDimension + SelectorAny::pick → SmallVec）
                        - R-E failover 执行层（消费 SmallVec，FailoverState 状态机）
                        - R-F stream probe（首 chunk 前 failover 安全网）
                        - R-C latency tracker + R-D affinity manager + R-G sticky moka fix（并行）
                        - RL-Core per-user/key RPM + RateLimitKind 分类 + detector
                        - RL-IP per-IP governor + IP allowlist
                        - RL-Adaptive AIMD（消费 K-B 喂的 RateLimitInfo）
                        - K-D quota probe（passive snapshot + active scheduler）
                  │
                  └──► Phase 5  Billing 切边（Rust 完全脱离 cost 计算）
                        - C-α UsageRecord schema 改造（BillingRecord 改名 + 6 维 UsageSnapshot + MarkupChainSnapshot）
                        - C-β EstimatePricing entity（Go writer，Rust read-only freeze 估算）
                        - C-γ 余额读写分工（Rust 仅写 frozen + events:counter 第 5 协议 consumer）
                        - C-δ usage:records DLQ + flusher 加固
                        - C-ε Go 契约文档（并行编写，不阻塞实施）
                        - MergedPermissions.markup_chain 接入（Bm1）
                  │
                  └──► Phase 6  Task Worker（依赖 Phase 3 adapter + Phase 5 UsageRecord ready）
                        - gw-task 业务契约 crate（DTO + Redis schema helper + lua 字符串 + submit/load/watch/cancel/claim/settle helper）
                        - gw-task-runtime 执行层（tokio loop A/B/C/D + 看门狗 leader election）
                        - gw-task-worker binary（axum :9091/metrics + :8081/livez-readyz；无业务 HTTP）
                        - gateway task 路径（POST /v1/tasks / GET / cancel / wait）
                        - A-T6 kiro vendor adapter
                        - A-T7 / codec S5 kiro EventStream codec
                        - settle.lua 改 release-only（B2）
                  │
                  └──► Phase 7  收口
                        - Prometheus 仪表盘示例
                        - ADR-T16 5 个部署矩阵 e2e 测试
                        - metric 改名最终化（gw_breaker_* 彻底删，gw_health_* 唯一）
                        - detailed_log 字段完整化（failover.candidates_count / keys_tried / exhausted_reason）
                        - docs/issues 全部 close 或迁
                        - 5-18 ADR 失配点（表 D）在 CLAUDE.md 加一行注
```

### §4.2 Phase 进入 / 退出门

| Phase | 进入条件 | 退出门 |
|---|---|---|
| **1** | (起点) | `cargo test --workspace` 绿；`gw-adapter` Noop trait + `gw-health` SlidingWindow + `gw-ledger` FreezeGuard trait + RequestFreeze release-only 单测覆盖；A1–A13 一刀切清单全部完成（CI grep §5.2 lint pass）；CLAUDE.md 三条 layering rules 已登记；business-split-refactor.md plan 内 v1 残留段已清理 |
| **2** | Phase 1 出口 | 9 对 wire 直转 roundtrip 测试绿（含多模态 / tools / thinking）；same-protocol passthrough bench 未回归（< 100 ns P50）；跨协议 < 8 µs P50；`extract_usage_any` 6 维输出测试覆盖三 codec；S3 多事件累积状态机单测；codex SSE event 改写通过 A-T5 集成测 |
| **3** | Phase 1 出口（codec 不阻塞 adapter） | 5 个 vendor adapter 单测 + 集成测；OAuthCache 懒刷新 + CAS lock 单测；K-B 喂 K-A `apply_cooldown` 链路 + ErrorOutcome.rate_limit_info 字段端到端绿（401/403/429/5xx 各一例）；`gw-adapter::Registry::for_vendor` 命中所有 5 vendor |
| **4** | Phase 3 出口（K-B / K-A ready） | 8 个 ScoringDimension 单测 + multi-score 组合测；R-E 7 个 failover 场景（同 key retry / 跨 key / 5xx / 4xx terminal / timeout / stream not retriable / 全失败）；RL-Core per-key skip 不计 K-A failure（mock 验证）；RL-Adaptive 学习 → override_key_rpm 闭环；R-F stream probe 在首 chunk 前切换；K-D quota probe 写回 vendor_key entity |
| **5** | Phase 4 出口（balance check 在 freeze 路径） | `usage:records` payload 6 维 + MarkupChainSnapshot 字段完整；`gw-proxy/filters/logging.rs` 已无 cost 计算代码（grep `cost_micro` 空）；`events:counter` consumer 跑通（mock Go 写入 → Rust 本地 deducted 更新）；启动期对齐函数从 Redis HGET base/deducted/frozen；mpsc backoff 行为测试；C-δ DLQ 三层兜底 |
| **6** | Phase 3 + Phase 5 出口 | gw-task 业务契约 lua 脚本单测；gw-task-runtime 状态机驱动 + lease watchdog leader election 集成测；ADR-T16 五个部署形态 e2e 全过（0G+1W 空跑 / 1G+0W 积压 / N×M 负载分散 / gateway-kill / worker-kill）；kiro EventStream roundtrip；worker 0→100 任务演练；settle release-only 验证不写 deducted |
| **7** | Phase 6 出口 | Prometheus 指标改名彻底（grep `gw_breaker_` 空 + `breaker_state` 空）；detailed_log 字段完整；docs/issues 8 项全 close 或迁；CLAUDE.md "5-18 ADR 失配点" 注记登记；5 个 ADR-T16 部署矩阵 testcontainers 测试在 CI 跑过 |

---

## §5 验收门

### §5.1 整合 spec 自身验收（写完即可验）

| # | 检查 | 通过条件 |
|---|---|---|
| 5.1.1 | 33 份 5-19 spec 顶部存在引用头注 | `git grep -l 'integrated by.*2026-05-20-v2-integration-design' docs/superpowers/specs/2026-05-19-*.md \| wc -l` = 33 |
| 5.1.2 | §2 表 A 一刀切清单完整覆盖 13 项 | 表 A 包含 A1–A13 全条目，依据列指向具体 spec / ADR |
| 5.1.3 | §2 表 B 6 条主决议每条带 (冲突点 + 决议 + 依据 spec / plan 节号) 三段 | 人工 review |
| 5.1.4 | §3 8 条全局不变量（C-1…C-8；C-0 是元规则）逐条可机器化 grep | C-1 grep 禁止依赖边 / C-2 错误码 grep / C-3 stream 名 grep / C-4 entity ns grep / C-5 schema_version 字段存在 / C-6 metric 前缀 grep / C-7 类型在 gw-core 而非业务 crate / C-8 配置文件无对端地址 |
| 5.1.5 | §2 表 D 5-18 失配点列出至少 3 项 | 实际列 5 项（D1–D5） |
| 5.1.6 | §4 phase 图 7 节点 + 进入/退出门表 7 行 | 视觉 review |
| 5.1.7 | spec 自身字数 | < 1200 行（含表）；> 1200 拆 |
| 5.1.8 | spec 头注 status / date / author / 关联 | 齐全 |

### §5.2 长期防御线 lint（每个 phase plan 必须复用）

| Lint 名 | 形式 | 触发条件（违例） |
|---|---|---|
| `crate_layering` | `git grep -nE 'task::\|billing::\|health::\|adapter::vendor' crates/{gw-core,gw-codec,gw-codec-macros,gw-store,gw-auth,gw-ledger}/src` 应为空 [^crate-layering-impl] (`gw-routing` excluded since Phase 1c reclassification — see [^routing-tier]) | 支撑层 crate 引入业务 namespace |
| `no_billing_crate` | `git grep -n 'gw-billing' Cargo.toml crates/*/Cargo.toml` 应为空 | gw-billing 复活 |
| `no_legacy_breaker` | `git grep -nE '\bBreakerState\b\|\bBreakerStore\b' crates/` 应为空（除 gw-health 内部 SlidingWindow） | breaker 残留 |
| `no_legacy_retry` | `find crates -path '*gw-routing/src/retry.rs'` 应为空 | retry.rs 残留 |
| `no_cost_micro` | `git grep -nE '\bcost_micro\b' crates/` 应为空 | cost 字段复活 |
| `no_billing_records_stream` | `git grep -n '"billing:records"' crates/` 应为空 | 旧 stream 名 |
| `no_patch_body` | `git grep -nE '\bfn patch_body\b\|\bBodyPatch::Splice\b\|\bBodyPatch::HeaderOnly\b' crates/` 应为空 | 旧 adapter trait |
| `no_freezeguard_settle` | `git grep -nE '\.settle\(.*actual\b\|FreezeGuard::settle\(' crates/` 应为空 | 旧 freeze API |
| `no_proxy_worker_dep` | `cargo metadata` 解析：`gw-proxy` 与 `gw-task-runtime` 互不出现在依赖闭包 | 执行体互依 |
| `no_worker_addr_config` | `git grep -nE 'worker[_-]?host\|worker[_-]?port\|TASK_WORKER_(URL\|HOST)' config/ crates/gw-gateway` 应为空 | gateway 配置出现 worker 地址 |
| `no_gateway_addr_config` | `git grep -nE 'gateway[_-]?host\|gateway[_-]?port\|GATEWAY_(URL\|HOST)' crates/gw-task-worker` 应为空 | worker 配置出现 gateway 地址 |
| `deploy_e2e_matrix` | 测试名 `e2e_deploy_matrix_{1g_0w,0g_1w,n_m,gateway_kill,worker_kill}` 全部存在 | ADR-T16 验收测试缺失 |
| `no_ir_runtime_struct` | `git grep -nE 'IrRequest::new\|IrResponse::new\|IrRequest::default\|IrResponse::default' crates/` 应为空 | codec S1 v2 schema 被运行时实例化 |
| `no_markup_product_scalar` | `git grep -nE '\bmarkup_product\b' crates/` 应为空（应为 markup_chain） | 旧 markup 单标量 |

[^crate-layering-impl]: **Implementation note** (Phase 1a `0fb1131`): the actual `scripts/check-invariants.sh` implements `crate_layering` as a `cargo metadata --no-deps` dependency-edge check filtered by `select(.kind == null)`, not a string `git grep`. The grep form false-positives at HEAD on legitimate occurrences like `tokio::task::` and `crate::billing::BillingRecord`. The dependency-edge form is strictly stronger (catches a forbidden edge before it shows up in source) and false-positive-free against dev/build dependencies.

### §5.3 滚动 review 节点

- Phase 1 实施 plan 写出后，在本 spec 顶部插一行 `> Phase 1 plan: docs/superpowers/plans/2026-05-2X-phase-1-scaffolding.md`；
- 每个 phase 完成时，本 spec 追加表 E 一行 "Phase N landed: <commit-sha>"；
- 任何破窗（新隐性冲突 / 新 ADR 撤销）→ 表 E 加 "Living amendment: <日期> / <冲突点> / <决议>"。

---

## §6 表 E — Living amendments（占位）

实施过程中遇到的新冲突 / 新决议追加在此。每条格式：

```
- YYYY-MM-DD  <题目>
  Why: <一句>
  How to apply: <落点 phase / spec / plan>
```

- 2026-05-20  Bm2 re-timing: `PickCtx::api_format` + Strategy uses `KeyHealthStore::is_available(key, fmt)`
  Why: integration spec §2.3 Bm2 placed this in Phase 4, but Phase 1c (A2) deletes `gw-routing::breaker.rs`, which forces strategies onto `KeyHealthStore::is_available` immediately. Without `api_format` on `PickCtx` the multi-dim lookup has no format input. The field lands in Phase 1c rather than Phase 4.
  How to apply: no follow-up needed — the field is plumbed end-to-end as of Phase 1c. Phase 4 R-AB / R-E build on top with no further structural change.

- 2026-05-20  `gw-routing` layer reclassification: supporting → business
  Why: `Strategy::pick(_, &KeyHealthStore, &InFlightStore, _)` is the new signature after Phase 1c A2. `KeyHealthStore` lives in `gw-health` (business layer), so `gw-routing` cannot be supporting-layer without violating the C-1 "support→business forbidden" rule. C-7 already designates `gw-health::KeyHealthStore` ↔ `gw-routing` selection as an intentional coupling point. The simpler resolution is to reclassify `gw-routing` to business. Dependency inversion via a `gw-core::KeyAvailability` trait remains viable for a future routing-only deployment.
  How to apply: §3 C-1 dependency graph + §5.2 `crate_layering` lint table updated (Phase 1c, commit `8032b08`). `CLAUDE.md` and `scripts/check-invariants.sh` carry the same classification. No code-side follow-up.

- 2026-05-20  A10 strict-form re-timing: schema rename + non_exhaustive strip moves to Phase 2
  Why: A10 prescribes `gw-core::ir → gw-core::schema` + `#[non_exhaustive]` on every IR struct, which forbids the public Default/constructor calls that the runtime via.rs handwritten From/TryFrom impls currently rely on. Phase 1d cannot strip those without simultaneously migrating the runtime onto wire→wire (codec S1 v2 N² conversions), which is Phase 2's scope. The Phase 1d-shipped pieces of S1 v2 (Mutations upgrade, WireAny enum, wire_convert_matrix! proc-macro scaffold) are framework-only and do not touch the runtime IR path. Phase 2 lands the rename + strip atomically with the wire→wire migration; the `no_ir_runtime_struct` lint activates at Phase 2 exit.
  How to apply: Phase 2 plan must include (1) the module rename, (2) the constructor strip, (3) activation of `no_ir_runtime_struct` in scripts/check-invariants.sh. Phase 1 exit gate text in §4.2 is read as "scaffolding complete + A1/A8/A9/A11 done; A10 strict form re-timed to Phase 2 exit per this amendment".

---

> 本 spec 是 v2 周期 33 份 5-19 spec 之间的整合权威。任何后续 phase plan 与具体 spec 实施都以本 spec §2 / §3 为锁定面；若发生新冲突，先在表 E 登记决议再实施。
