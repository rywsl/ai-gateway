# 高性能 LLM 网关 — 架构设计文档（v2026-05-18）

- **日期**：2026-05-18
- **状态**：Draft（待审阅）
- **作者**：协作设计（Claude + 用户）
- **取代**：[2026-05-14-llm-gateway-design.md](./2026-05-14-llm-gateway-design.md)
- **驱动需求**：[`only-gateway-frd.md`](../../../only-gateway-frd.md)（以 FRD 为权威需求源）
- **范围**：Rust 端高性能 LLM API 中转网关 / 协议层归一化 / 号池负载均衡 / JWT 鉴权 / 实时计费（估算冻结） / 与 Go 管理面（含 PgSQL 持久化）通过 Redis 协同
- **不在范围**：Go 管理面实现（充值、用户管理、报表、UI）、仅定义其与 Rust 之间的契约

> **本次改动说明（vs 2026-05-14）**：
> - **新增 crate**：`gw-codec-macros`（proc-macro = true，独立成 crate 是 Rust 语言强约束）—— derive 宏自动生成 IR ↔ wire-format 双向转换代码，全部 `#[inline(always)]`，编译期固化字段映射，热路径零运行时字段查找。
> - **新增 ADR**：
>   - ADR-018 Crate 分层规则（9-crate DAG + 依赖方向硬约束 + 边缘 crate v1 不实现）
>   - ADR-019 协议层 IR + ProtocolCodec trait + **derive 宏驱动的转换**（`#[derive(IrConvert)]` 取代手写 sonic-rs 字段抽取）
>   - ADR-020 错误层 ErrorIR + 稳定错误码字典（23+ 个 GW_* 稳定码）
>   - ADR-021 Vendor + VendorKey 扩列 + 7 状态机（Vendor / VendorKey / ModelGroup 三层 entity）
>   - ADR-022 重试策略（SameKey / CrossKey / Mixed + 幂等保护 + 流式不重试）
>   - ADR-023 用户分组 + 子账户 + 五级权限合并（白名单交集 / 数值取严 / 倍率连乘）
>   - ADR-024 多级限流 + 单请求 hard cap + 并发流式上限（governor 本地令牌桶 + Redis 周期同步）
>   - ADR-025 安全风控扩展点（v1 IP 黑白名单 + 签名；内容审计 / 敏感词通过 trait 预留）
>   - ADR-026 双级日志 + 写入时脱敏 + 响应头契约（simple_log 365d + detailed_log 7d）
>   - ADR-027 Debug 模式（请求级 opt-in + 全链路 phase 时间 + jemalloc 内存增量 + 详细日志，性能瓶颈定位用）
> - **改写 ADR**：
>   - ADR-003（4 策略 → 8 策略 + Fallback 叠加；明确 gw-store/gw-auth/gw-routing 职责边界）
>   - ADR-004 / ADR-015（事后扣减 → 三阶段估算冻结 + 多退少补 + FreezeGuard RAII；三层余额 base/deducted/frozen）
>   - ADR-009（3 类超时 → 4 类：connect / read-idle / total-nonstream / stream_total）
>   - ADR-017（api_key 明文 → AES-256-GCM + KMS + KEK 轮换 + 解密时机 + 失败处理）
> - **章节扩展**：§4 数据模型新增 9 个 entity 与 5 个 counter；§5 生命周期串联 31 个 phase 步骤；§6 9-crate workspace + derive 宏依赖；§7 性能预算新增协议层 SLA；§8 PgSQL 新增 7 张表；§9 metrics 新增 codec/retry/freeze/ratelimit/security/kms 6 类；§10 三端口分离 + preStop hook + 健康探针。

---

## 目录

1. [背景与目标](#1-背景与目标)
2. [总体架构](#2-总体架构)
3. [关键架构决策（ADR）](#3-关键架构决策adr)
4. [数据模型](#4-数据模型)
5. [请求生命周期](#5-请求生命周期)
6. [模块结构与库选型](#6-模块结构与库选型)
7. [性能预算与容量规划](#7-性能预算与容量规划)
8. [PgSQL 与 Go 站契约](#8-pgsql-与-go-站契约)
9. [可观测性](#9-可观测性)
10. [运维与故障处理](#10-运维与故障处理)
11. [参考资料](#11-参考资料)

---

## 1. 背景与目标

### 1.1 业务场景

构建一个 **LLM 中转 / 聚合网关**，对外暴露 OpenAI / Anthropic / Gemini 兼容的 API（含 SSE 流式），对内：

- **协议归一化**：N 个客户端协议 × M 个上游协议 → 内部统一 IR（RequestIR / ResponseIR / StreamEventIR / ErrorIR），新增协议仅需新增一个 `ProtocolCodec` 实现（FRD §2）；
- 维护一个**多供应商号池**（Vendor → 多 Vendor Key），按可插拔策略做负载均衡；
- 对调用方进行 **JWT / API Key 鉴权**，识别用户身份、用户分组、子账户，按五级权限合并（FRD §6）；
- **实时计费 — 估算冻结模式**：请求前按 `(input + max_tokens) × 单价 × 倍率连乘` 估算冻结，请求后多退少补；余额不足即时 402，不发往上游（FRD §10.3）；
- 与 **Go Web 站** 通过 Redis 协同，Go 站负责充值、号池管理、报表，PgSQL 是最终账目持久化层；
- 极致追求性能：目标 **2k+ QPS、1 万+ 并发流式连接**，同协议透传引入开销 P99 ≤ 800µs（FRD AC-2.3-1）。

### 1.2 设计原则

1. **热路径零额外网络 IO**：JWT 验签、号池路由、配额检查、估算冻结全部内存操作；冷路径（缓存 miss）才访问 Redis / PgSQL。同请求内每种资源最多发生一次 miss-fill。
2. **职责单一**：Rust 网关是**纯转发层**，不提供任何管理 UI/API；管理面（Go 站）独立维护。
3. **协议 IR 统一**：所有路由 / 改写 / 计费 / 日志基于内部 IR；客户端协议与上游协议解耦，新增协议不触碰已有协议（FRD §2.1）。
4. **写权威单点**：用户、号池、计价表的权威 writer 是 Go（背靠 PgSQL）；Rust 通过通用协议读取 + 增量同步。Rust 唯一可写的 Redis key 是计费扣账、流水、Key 状态机的 `rate_limited` 标记。
5. **通用交互协议优于业务特化**：Rust ↔ Go ↔ Redis 之间归约为四类（Entity / Counter / Append log / Alert），新增业务只是新 namespace（详见 ADR-013）。
6. **完全懒加载、零预热**：进程启动后立即接流量，所有资源在被使用时才拉（详见 ADR-005）。
7. **敏感数据零暴露**：`api_key` 字段级 AES-256-GCM 加密（KEK 经 KMS），仅在内存中即用即毁，不进日志 / 监控 / 调试输出（ADR-017）。
8. **可演进**：策略、协议、tokenizer 审计模块按 crate 边界拆分；payload 协议预留 `enc` 字段，业务调整不需重构核心。
9. **保留备选方案**：本文档记录被否决的方案与否决理由，未来环境变化时无需从零调研。

### 1.3 非目标

- **不**实现请求级 LLM 响应缓存（LLM 响应几乎不可复用）。
- **不**实现热加载策略代码（策略变更走重新部署）。
- **不**在 Rust 提供管理 HTTP API（"纯转发层"约束）。
- **不**在 Rust 维护用户登录/注册（Go 完成，Rust 仅本地验签）。
- **不**做 Tokenizer 本地复算用于生产计费（仅作为可选审计模块、独立 crate、feature gate；详见 FRD §14）。

---

## 2. 总体架构

### 2.1 系统拓扑

```
                              ┌──────────────────────┐
                              │   Go Web Admin       │
                              │ (PgSQL writer side)  │
                              │  上线后写 entity:*   │
                              └─────────┬────────────┘
                                        │  写 entity:* + counter:* + XADD events:*
                                        ▼
                              ┌──────────────────────────────────────┐
                              │              Redis                    │
                              │ • entity:<ns>:<id>           HASH     │
                              │     ns ∈ {user, user_group, vendor,   │
                              │            vendor_key, pricing, jwks, │
                              │            apikey, model_group, ...}  │
                              │ • counter:<scope>:<id>:<f>   STRING   │
                              │     balance_micro / used_count /      │
                              │     tokens_today_<YYYYMMDD>           │
                              │ • events:entity              STREAM   │
                              │ • events:counter             STREAM   │
                              │ • billing:records            STREAM   │
                              │ • alerts                     STREAM   │
                              └────┬─────────────────────────────┬───┘
                                   │ XREADGROUP / GET (lazy)     ▲
                                   │                             │ XADD billing/alerts
                                   │                             │ HINCRBY counter
                                   │                             │ HSET entity:* (TTL, ADR-016)
            ┌──────────────────────┴─────────────────────────────┴──────────────────────┐
            │                          Rust Gateway (pingora)                            │
            │                                                                              │
            │   ┌─────────────────────────────────────────────────────────────────┐      │
            │   │  入口 / 业务 Filter                                             │      │
            │   │  ProxyHttp::{early_request_filter, request_filter,              │      │
            │   │              upstream_peer, upstream_request_filter,            │      │
            │   │              response_body_filter, logging}                     │      │
            │   └────────────┬────────────────────────────────┬───────────────────┘      │
            │                │ ① codec.detect → parse → IR    │ ⑥ response codec        │
            │                ▼                                ▼                          │
            │   ┌─────────────────────────┐  ┌─────────────────────────────────────┐    │
            │   │  ProtocolCodec layer    │  │  Error normalization (ErrorIR)      │    │
            │   │  OpenAI / Anthropic /   │  │  parse_error / render_error         │    │
            │   │  Gemini (双向 + 透传)   │  │  稳定错误码字典 GW_*                │    │
            │   └────────────┬────────────┘  └─────────────────────────────────────┘    │
            │                │ ② IR + RequestCtx                                          │
            │                ▼                                                            │
            │   ┌──────────────────────────────────────────────────────────────────┐    │
            │   │  Auth → 五级权限合并 → 多级限流 → 估算冻结                       │    │
            │   │  (JWT / APIKey) → (User × Group × SubAccount × Key × Global)     │    │
            │   │  → DEDUCT[uid].try_freeze(estimate) → USER_CIRCUIT 检查          │    │
            │   └──────┬────────────────────────────────────────┬──────────────────┘    │
            │          │ ③ pass / 4xx 拒绝                       │                       │
            │          ▼                                         │                       │
            │   ┌──────────────────────────────────┐  ┌─────────────────────────────┐  │
            │   │ ModelGroup.get_candidate_keys    │  │ EntityStore (统一懒加载)    │  │
            │   │ → Strategy.pick (8 策略 + Fallback)│ │ Moka<K, Versioned<V>>       │  │
            │   │ → Retry executor (同/跨/混合)    │  │ user / user_group / vendor /│  │
            │   └──────┬───────────────────────────┘  │ vendor_key / model_group /  │  │
            │          │ ④ KeyEntry (decrypted)       │ pricing / jwks / apikey      │  │
            │          ▼                              └──────────────┬──────────────┘  │
            │   ┌──────────────────────────────────┐                 │ miss → Redis     │
            │   │ Upstream HTTP (pingora HttpPeer) │                 │   miss → sqlx PG │
            │   │ H2 多路复用 / SSE 流式 / 4 类超时│                 ▼                  │
            │   └──────────────┬───────────────────┘  ┌─────────────────────────────┐  │
            │                  │ ⑤ chunk → parse_stream│ Billing Worker (mpsc → pipe)│  │
            │                  ▼                       │ logging: DEDUCT 多退少补    │  │
            │   ┌──────────────────────────────────┐  │ → INCRBY counter:user:*     │  │
            │   │ Response codec / SSE usage 抽取 │  │ → XADD billing:records      │  │
            │   └──────────────────────────────────┘  └─────────────────────────────┘  │
            └────────────────────────────┬────────────────────────────┬──────────────────┘
                                         │                            │
                                         ▼                            ▼
                                ┌──────────────────┐          ┌──────────────────┐
                                │ 上游             │          │   PostgreSQL     │
                                │ (OpenAI 兼容 /   │          │ Rust 直连冷加载  │
                                │  Anthropic /     │          │  (sqlx, 16 conn) │
                                │  Gemini / ...)   │          │                  │
                                │   HTTP/2 / SSE   │          └──────────────────┘
                                └──────────────────┘
```

### 2.2 组件分层

| 层 | 职责 | 主要类型 |
|---|---|---|
| **入口** | TCP/TLS 接受、HTTP/1.1+H2 解码 | pingora `HttpProxy` |
| **协议层** | 协议识别、双向 codec、零成本同协议透传 | `ProtocolCodec` trait + `OpenAi`/`Anthropic`/`Gemini` impls（ADR-018） |
| **错误层** | 上下游错误归一化、稳定错误码字典 | `ErrorIR`、`GW_*` 错误码（ADR-019） |
| **业务 Filter** | JWT/APIKey、IR 抽 model、五级权限合并、多级限流、估算冻结、ctx 准备 | `ProxyHttp::request_filter` |
| **统一缓存** | 所有元数据资源经 Entity 协议懒加载 | `EntityStore`、`CounterStore` |
| **路由** | model → ModelGroup → 候选 VendorKey → Strategy.pick + Fallback → Retry executor | `Strategy` trait（ADR-003）、`RetryExecutor`（ADR-021） |
| **改写** | Authorization（按 Vendor.auth_template）、Host、Path、model_mapping | `ProxyHttp::upstream_request_filter` |
| **响应观测** | SSE 增量解析 usage、跨协议 chunk codec | `ProxyHttp::response_body_filter` |
| **结账** | 估算冻结 → 实际扣减 → 多退少补、断路判定、入 mpsc | `ProxyHttp::logging` |
| **后台** | Stream 消费、计费 flush、健康探活、Key 状态机扫描 | 独立 tokio task |

### 2.3 端口拓扑（FRD §15.2.7）

- `server.business_port`（默认 **8000**）：业务流量入口（HTTP/HTTPS + SSE）
- `server.metrics_port`（默认 **9090**）：Prometheus `/metrics` 暴露，仅内网
- `server.health_port`（默认 **8080**）：K8s 探针 `/livez` / `/readyz`，仅内网

三端口分离的语义、preStop hook 与 ready 判定标准在 batch 2 §10 中详细描述。

---

## 3. 关键架构决策（ADR）

> 每条 ADR 都遵循"决定 / 备选 / 否决理由"三段式。备选方案保留以供未来重新评估。
>
> **本文档（v2026-05-18）改动总览**：
> - 改写：ADR-003、ADR-004、ADR-015、ADR-017
> - 新增：ADR-018、ADR-019、ADR-020、ADR-021、ADR-022
> - 保留（沿用 v2026-05-14）：ADR-001、ADR-002、ADR-005~ADR-014、ADR-016
> - 待 batch 2 改写：ADR-009（3 类超时 → 4 类超时）；批次 2 新增 ADR-023~ADR-025

### ADR-001 代理框架选用 Pingora（沿用）

- **决定**：使用 `pingora` + `pingora-proxy` 0.8+，所有上下游 HTTP 处理走 pingora 内置连接池与流式转发，**不引入 reqwest 进入热路径**。
- **备选 A**：hyper + tower 自建。**否决**：连接池、零拷贝、SSE、HTTP/2 多路复用都要重写，且要长期维护。
- **备选 B**：axum 业务层 + 内部 HTTP 调 pingora 或 reqwest。**否决**：进程内多一跳序列化反序列化，2k+ QPS 下尾延迟会增加 5–15ms，违背性能目标。
- **未来重评信号**：如果 QPS 长期 < 200 且业务复杂度上升明显，可重新评估 axum 单层方案。

### ADR-002 Rust 仅做纯转发，无管理 API（沿用）

- **决定**：Rust 不暴露任何 admin/控制接口，全部交互通过 Redis（消费 events:*、写 billing:records / alerts）。健康探针端口（FRD §13.3）的 `/livez` `/readyz` 不视为 admin（仅返回 200/503，不接受写入）。
- **备选**：在 Rust 暴露只读 `/admin/keys/status` 等接口给 Go 拉取实时状态。**否决**：增加复杂度且与 metrics 重复（Prometheus pull 已能覆盖）。
- **未来重评信号**：如果调试需要外部触发"刷新某个 Key 状态""强制重置熔断器"，可在网关上单独加 admin 监听端口（与业务端口、健康端口都分离）。

### ADR-003 路由策略 = trait + 8 实现 + Fallback 叠加层（改写 / 取代 v2026-05-14 ADR-003）

- **决定**：定义 `Strategy` trait，提供 **8 种主策略 + 1 个 Fallback 叠加层**，按模型分组（`ModelGroup`）独立配置；选择哪种通过 `entity:model_group:<name>` 中的 `strategy` 字段决定（Entity 懒加载，5s 内热生效，无需重启）。
  - 8 种主策略（对齐 FRD §7.3）：
    | 策略名 | enum 变体 | 优先级 | 实现要点 |
    |---|---|---|---|
    | 缓存亲和 | `CacheAffinity` | P2 | 对消息列表 SHA-256 + 采样参数；命中 Key 不可用降级为分组默认策略 |
    | 固定顺序 | `FixedOrder` | P0 | 按 `priority` 升序尝试；故障转移由 Fallback 驱动 |
    | 轮询 | `RoundRobin` | P0 | `DashMap<GroupId, AtomicUsize>`；每 60s flush 计数器到 Redis 避免重启偏斜 |
    | 负载均衡 | `LoadBalance` | P0 | `score = α·in_flight + β·avg_latency + γ·error_rate`；最低分胜出，同分降级为 RoundRobin |
    | 成本优先 | `CostFirst` | P1 | 按 Key 的 model cost 排序；5% 容差阈值防抖动；错误率超阈值临时排除 |
    | 延迟优先 | `LatencyFirst` | P1 | 基于最近 5 分钟 TTFT P50；3σ 排除异常值；负载 > 90% 切次低延迟 |
    | 加权轮询 | `WeightedRR` | P0 | 平滑加权轮询（SWRR）；权重 0 = 临时禁用；权重热修改 15s 内生效 |
    | 粘性路由 | `Sticky` | P2 | 基于 `X-Conversation-Id` 头 + `conv:<id> → key_id` 映射（TTL 30 min） |
  - **Fallback 叠加层**（FRD §7.3.8，P0）：任意主策略 picked 失败后由 `RetryExecutor`（详见 ADR-021）驱动：
    - 触发条件：HTTP 5xx、429、超时、连接错误、健康检查失败。
    - 排除条件：HTTP 4xx（非 429）。
    - 顺序：当前 Key → 同分组其他可用 Key（按 `priority` 升序）。
    - 跨供应商 Fallback：分组配置 `cross_vendor_fallback` 开关，默认关闭（保持厂商一致）。
- **trait 签名**：
  ```rust
  #[async_trait::async_trait]
  pub trait Strategy: Send + Sync + 'static {
      fn name(&self) -> &'static str;
      fn pick<'a>(
          &self,
          candidates: &'a [Arc<VendorKey>],   // 已经被 get_candidate_keys 过滤
          breakers: &DashMap<KeyId, Arc<BreakerState>>,
          ctx: &PickCtx,
      ) -> Option<&'a Arc<VendorKey>>;
  }
  ```
  - `candidates` 是 `ModelGroup.get_candidate_keys(model, target_protocol)` 的返回（已过滤 `status != active`、IP/模型权限不通过、`rate_limited` 状态、`in_flight` 超限的 Key）。
- **职责边界（crate 视角，对齐 ADR-018）**：
  - `gw-store` 提供 `ModelGroup::get_candidate_keys(model, target_protocol, perm: &MergedPermissions, breakers: &BreakerStore) -> Vec<Arc<VendorKey>>`：负责按 entity 字段过滤（`status`、`accessible_models`、`accessible_endpoints`、`in_flight`、`rate_limited`）。
  - `gw-auth` 在 request_filter 阶段产出 `MergedPermissions { allowed_models, allowed_channels, ip_allowed_keys }`（五级权限合并结果，详见 ADR-023），作为入参传给 `get_candidate_keys`。
  - `gw-routing` 的 `Strategy::pick` 只对**已过滤完成**的候选列表打分排序；它**不**重新校验权限——避免在两个 crate 里重复实现同一份权限语义。
  - 调用顺序（在 `gw-proxy::upstream_peer` 中编排）：
    ```
    perm = gw-auth::merge(user, groups, sub_account, key, global)
    candidates = gw-store::ModelGroup::get_candidate_keys(model, proto, &perm, &breakers)
    chosen = gw-routing::Strategy::pick(&candidates, &breakers, &PickCtx { user, model, ... })
    ```
- **备选 A**：Lua / Wasm 脚本动态加载策略。**否决**：动态化在 2k+ QPS 上引入解释器开销；策略 8 种已覆盖业务场景，新增频率低于月级。
- **备选 B**：单一固定策略（写死代码）。**否决**：FRD §7.3 明确"按分组选策略"，不同模型分组场景差异大（成本敏感 vs 延迟敏感）。
- **备选 C**：把 Fallback 作为一种独立策略与其他 8 种并列。**否决**：Fallback 是叠加语义（每种主策略都需要 fallback 兜底），并列会导致组合爆炸。Fallback 是 `RetryExecutor` 的策略，主 `Strategy` 只管"选一个"。
- **未来重评信号**：策略变更频率上升到天级、或需要 A/B 测试不同策略时，再考虑引入动态化（如把策略 ID 放在请求级 ctx）。

### ADR-004 实时计费 = 估算冻结 + 多退少补（改写 / 取代 v2026-05-14 ADR-004）

- **决定（取代 v2026-05-14 ADR-004 的"事后扣减"模型）**：
  - **三阶段计费**：
    1. **请求前估算冻结**（request_filter 阶段）：
       ```
       estimate_micro = ceil(
           estimate_input_tokens × input_price_micro_per_token
         + max_tokens_capped × output_price_micro_per_token
       ) × markup_product
       ```
       - `estimate_input_tokens`：来自 Codec 的 `estimate_input_tokens(ir)` 默认实现，按字符/字节经验系数（FRD §5.6 + §14）。
       - `max_tokens_capped = min(client.max_tokens, model.max_output, group.max_tokens_cap)`（FRD §7.6）。
       - `markup_product = model.markup × user.markup × group.markup × distributor.markup`（FRD §10.2，连乘）。
       - 校验：`real = base_micro - deducted_micro - frozen_micro_self`；`real < estimate` → `GW_QUOTA_BALANCE_ZERO`（402），**不发往上游**。
       - 通过校验后：`DEDUCT[uid].frozen_micro.fetch_add(estimate)` + 在 `ReqCtx` 中保存 `freeze_amount = estimate`。
    2. **响应后实际扣减**（logging 阶段）：
       ```
       actual_micro = ceil(
           usage.input_tokens × input_price_micro_per_token
         + usage.output_tokens × output_price_micro_per_token
       ) × markup_product
       ```
       - `usage` 来源优先级（FRD §10.6）：上游显式 `usage` > Tokenizer 估算扩展（如启用） > 保守上限（`max_tokens × usage_fallback_ratio`，默认 0.1）。
       - `BillingRecord.usage_source ∈ { upstream, estimated, fallback_conservative }`（FRD §14.3）。
    3. **多退少补 + 解冻**（logging 阶段同步执行）：
       ```rust
       DEDUCT[uid].frozen_micro.fetch_sub(freeze_amount);          // 解冻
       DEDUCT[uid].deducted_micro.fetch_add(actual_micro);         // 实际扣账
       mpsc.try_send(BillingRecord { actual_micro, freeze_amount, usage_source, ... });
       ```
       - 后台 worker 每 100 条 / 200ms 用 Redis PIPELINE flush：
         ```
         HINCRBY counter:user:<uid>:balance_micro -<actual_micro>
         HINCRBY counter:user:<uid>:tokens_total <total_tokens>
         HINCRBY counter:user:<uid>:tokens_today_<YYYYMMDD> <total_tokens>
         EXPIRE counter:user:<uid>:tokens_today_<YYYYMMDD> 129600   # 36h
         XADD billing:records * ...
         ```
- **失败请求的冻结金额处理**（FRD §10.4）：
  | 请求结果 | 实际扣减 | 冻结金额 |
  |---|---|---|
  | 2xx 成功 | 按实际 usage 扣 | 解冻差额 |
  | 4xx 非 429 | 不计 | 全额解冻 |
  | 429 上游限流 | 不计 | 全额解冻，触发跨 Key 重试 |
  | 5xx 上游服务端错误 | 不计 | 全额解冻 |
  | 超时 / 网络异常 | 不计 | 全额解冻 |
  | 流式中断（已输出 token） | 按已输出 token 扣 | 按实际结算 |
  | 重试成功 | 按最终成功 Key 计 | 多退少补（冻结只发生一次） |
  | 重试全失败 | 不计 | 全额解冻 |
- **断路语义**：
  - 估算冻结后 `real ≤ 0`（或更严格 `real < estimate_threshold`）→ 立即打开 `USER_CIRCUIT[uid]` + 异步 `XADD alerts type=balance_zero ...`（不阻塞响应）；下一请求秒级 402，不查 Redis。
  - 充值事件（events:counter）到达且 `real > threshold` → 移除 `USER_CIRCUIT[uid]`，刷新 `base_micro`。
- **冻结释放的 RAII 保证（避免 frozen leak）**：
  - 冻结金额必须在请求结束时**无条件**释放（无论成功、失败、panic、客户端断开），否则用户额度会被错误锁住直到进程重启。
  - 实现：在 `ReqCtx` 上挂一个 `FreezeGuard { uid, freeze_amount, settled: AtomicBool }`，`Drop` 时检查 `settled`：
    ```rust
    impl Drop for FreezeGuard {
        fn drop(&mut self) {
            if !self.settled.load(Ordering::Relaxed) {
                // logging phase 未执行（panic / pingora 提前 short-circuit / 客户端断开早期）
                DEDUCT.get(&self.uid).map(|s| {
                    s.frozen_micro.fetch_sub(self.freeze_amount as i64, Ordering::Relaxed);
                });
                metrics::counter!("gateway_freeze_leak_recovered_total").increment(1);
            }
        }
    }
    ```
  - 正常路径（logging phase）调用 `freeze_guard.settle(actual_micro)` → 内部完成"frozen -= freeze_amount, deducted += actual_micro"并置 `settled = true`，跳过 Drop 路径。
  - `ReqCtx` 由 pingora 的 `Session::context` 持有，请求生命周期结束时由 Pingora 自动 drop（含 fail_to_proxy、客户端断开、内部 panic 被 catch 三种路径），RAII 保证生效。
  - 监控：`gateway_freeze_leak_recovered_total` 长期非零 → 说明有路径没走 logging（视为缺陷待修复）。
- **DeductionState 三字段**：
  ```rust
  pub struct DeductionState {
      pub base_micro: AtomicI64,     // 最近一次 events:counter 同步的绝对值
      pub deducted_micro: AtomicI64, // 本进程已扣未 flush 的累计扣费
      pub frozen_micro: AtomicI64,   // 本进程已冻结未结算（in-flight 请求）的累计估算
  }
  ```
- **备选 A（已采用）**：估算冻结 + 多退少补。
- **备选 B（v2026-05-14 ADR-004，已废弃）**：事后扣减（无 frozen 字段）。**否决**：FRD §10.3 明确要求"请求前估算冻结，不发往上游"语义；事后扣减允许已经透支的请求继续发往上游浪费上游配额。
- **备选 C**：热路径每请求 `DECR counter:user:<id>:balance_micro`。**否决**：2k+ QPS × Redis RTT（0.5ms）= 1ms+ 延迟 + Redis CPU 单核打满风险。
- **备选 D**：纯 channel + 批量 flush，无本地 deducted/frozen。**否决**：用户花完最后一块钱后还能跑数百请求，财务损失不可接受；FRD §10.3 用户已明确否决此方案。
- **已知容忍偏差**：充值与计费 flush 并发可能让 Redis 余额低于真实账面，规模上限 = batch flush 间隔 × 单用户瞬时并发 × 单请求成本。Go 侧日终对账修正（FRD §10.3）。
- **未来重评信号**：单用户欺诈风险上升、对实时账面要求秒级精度时，可改为 Lua 脚本原子扣账（参见 ADR-015 重评信号）。

### ADR-005 资源加载策略 — 完全零预热（全懒加载）（沿用）

- **决定**：进程启动后立即 accept 流量，**所有资源**（user / user_group / vendor / vendor_key / model_group / pricing / jwks / apikey）100% 懒加载，**不**做任何预热、不维护任何 warm 列表、不区分 Bounded/Unbounded。所有缓存 miss 走统一加载链（详见 ADR-016）：
  ```
  Moka.get_with(key, async {
      Redis(Entity 协议).get(ns, id)
        .or_else(sqlx → PgSQL SELECT)
        .map(|raw| Versioned::decode(raw))
  })
  ```
  全链路任一节点失败 → 503 给当前请求。
- **接受代价**：
  - 进程冷启动后第一波请求集中 miss → 瞬时 Redis HGETALL 突发；Redis 单核可轻松扛数十并发，可接受。
  - K8s 滚动更新时多 pod 同时上线，跨 pod 重复拉同一资源（无分布式 single-flight）；首批 miss 会落到 PgSQL，但 Rust 会回写 Redis（带 TTL，详见 ADR-016），后到的 pod 命中 Redis。
  - 第一个使用某 model_group 的用户首请求延迟略高（多一次 Redis RTT，可能再多一次 PgSQL 查询 ≤ 50ms）。
- **备选 A**：Bounded（号池/计价/JWKS）启动全量预加载、Unbounded（用户）懒加载。**否决**：引入"哪类资源何时加载"的二分类，每种新资源都要决定归类；用户答复明确选 A=简单有效；号池总体即便上千 entry 也不需要常驻——"用户没用就不用进内存"。
- **备选 B**：Warm hint 配置（toml 列出预热项）。**否决**：多一份配置漂移源；新增 group/model 时人要记得更新；启动失败语义需额外讨论。
- **备选 C**：Redis hot 列表自动 warm。**否决**：引入新协议、Go 侧要算热度、复杂度收益比一般。
- **备选 D**：Entity 自描述 warmlist。**否决**：可演进但实际等价于 B 的动态版本，零预热已足够。
- **备选 E**：B + D fallback 多层。**否决**：复杂度最高。
- **未来重评信号**：冷启动突刺造成 P99 显著恶化（如 1s+）、Redis 在新部署期间过载、上线节奏从滚动变为整体替换时，再考虑加 warm hint 或 hot 列表。

### ADR-006 Versioned Cache + Stream 事件单调应用（沿用）

- **决定**：所有 Entity 携带 `version: u64`，**权威来源 = PgSQL 表的 `version BIGINT NOT NULL DEFAULT 0` 列**。
  - **当前阶段**（Go 未上线）：Rust 仅读 version 列；写入由数据库迁移脚本 / 人工 SQL 完成，version 通过 `UPDATE ... SET version = version + 1` 模式维护。
  - **Go 上线后**：Go 在事务内 `UPDATE ... SET version = version + 1 RETURNING version`，将拿到的 V_NEW 同步写入 Redis HASH `v` 字段与 events:entity 事件载荷。
  Rust 读取后封装为 `Versioned<T>`，所有更新走 `upsert_if_newer`：**严格大于才覆盖**，等于或小于丢弃。
- **解决三种竞态**：(1) 事件先到、懒加载随后返回旧值 → 旧的丢；(2) 懒加载先到、事件随后到 → 新的覆盖；(3) 并发懒加载 → Moka `get_with` single-flight + version 双保险。
- **Stream 事件应用规则**：consumer 收到事件后先查对应 Moka，**若该 entity 不在缓存中（用户从未访问过），事件直接 XACK 丢弃，不主动加载**——它在缓存里就没必要 occupy memory，等下一个用户来时再走懒加载链拿到最新版本。
- **备选 A**：事件直接 `invalidate(key)`，下次请求重新拉。**否决**：失效到下次访问的窗口内请求会拿旧数据。
- **备选 B**：事件触发主动加载（即使缓存中没有也加载进来）。**否决**：违背懒加载原则，造成缓存膨胀。
- **备选 C**：version 来源 = `updated_at::epoch_micros`。**否决**：DB 时钟漂移 / NTP 抖动可能让 v 不严格单调；trigger 维护比显式列复杂。
- **备选 D**：version 来源 = PostgreSQL 系统列 `xmin`。**否决**：xmin 在 VACUUM FREEZE / 逻辑复制 / pg_dump 后会变，跨 pod 仲裁会出错。
- **未来重评信号**：跨表全局排序成为需求时（极少见），可用 Snowflake 或 sequence 集中分配。

### ADR-007 Stream Consumer 启动对齐 = LAST_ID 锚点（沿用）

- **决定**：
  1. 启动时 `XINFO STREAM events:entity / events:counter` 取 `last-generated-id` → 记 `X_LAST_*`；
  2. 不做全量预加载（与 ADR-005 一致）；
  3. `XREADGROUP GROUP <instance_id> <consumer> COUNT N BLOCK ms STREAMS events:* X_LAST_*`，从锚点之后开始消费。
- **作用**：保证 consumer 启动到第一次懒加载之间的事件不会丢——若懒加载晚于 X_LAST 之后的某事件，懒加载从 Redis 读到的就已经是事件应用后的新值（Go 写主表与 XADD 是同事务/pipeline）；若懒加载早于事件，事件到达后会通过 versioned upsert 覆盖。
- **备选**：从 `$` 起读后续。**否决**：可能丢失启动瞬间的事件。
- **未来重评信号**：无（这是数据正确性问题）。

### ADR-008 熔断 = 每实例独立判断（沿用）

- **决定**：
  - 数据结构：`DashMap<KeyId, Arc<BreakerState>>`，`BreakerState { failure_window, circuit, open_until, in_flight }`；
  - 算法：30s 滑动窗口失败率 > 50% 且样本 ≥ 20 → open；open 60s 后转 half-open，放行 1 请求探活；探活成功转 Closed，失败回 Open 并 60s 计时重置（FRD §8.3）；
  - `in_flight` 上限：单 Key 同时处理请求数上限默认 200（可配 `breaker.in_flight_per_key`），达上限时该 Key 暂时不可路由（不算熔断）；
  - 失败定义：HTTP 5xx、超时、连接错误。HTTP 4xx 不计入失败率。客户端断开（`ErrorCategory::Cancelled`，见 ADR-019）不计入。
- **备选**：通过 Redis 共享熔断态（例如 `pool:circuit:<key_id>`）。**否决**：故障检测增加 Redis RTT；多实例独立判断在 pod 数量级 ≤ 100 时探活流量代价可接受。
- **未来重评信号**：pod 规模 > 100 或 key 故障率被探活流量显著放大时，改共享。

### ADR-009 超时 = 4 类按 Model / Vendor 配置（改写 / 取代 v2026-05-14 ADR-009）

> **驱动需求**：FRD §3.3 / §15.2.2。v2026-05-14 仅 3 类超时，对流式 idle 与流式总耗时不区分，无法表达"流式可以慢但不能无限挂"语义。

- **决定（4 类超时）**：
  | 超时 | 适用 | 默认 | 触发后行为 | 配置层级 |
  |---|---|---|---|---|
  | `connect_timeout_ms` | 全部 | **5000** | 立即返回 `GW_UPSTREAM_TIMEOUT`，可重试 | 全局 / per-Model / per-Vendor |
  | `read_timeout_ms`（流式 idle） | 流式 | **30000** | 立即返回 SSE error event + 关闭流，不重试 | 全局 / per-Model |
  | `total_timeout_ms`（非流式总耗时） | 非流式 | **60000** | 立即返回 `GW_UPSTREAM_TIMEOUT`，可重试 | 全局 / per-Model |
  | `stream_total_timeout_ms`（流式总耗时兜底） | 流式 | **600000** | 立即返回 SSE error event + 关闭流，不重试 | 全局 / per-Model |

- **每超时的语义细则**：
  - **connect_timeout_ms**：TCP/TLS 握手完成截止时间。pingora `HttpPeer::connection_timeout`。
  - **read_timeout_ms（流式 idle）**：两次接收 chunk 的最大间隔。每收到一个新 chunk 重置计时器。pingora `HttpPeer::read_timeout` 在流式时按 idle 语义使用。
  - **total_timeout_ms（非流式）**：从 connect 完成到收到完整响应体的总时间。pingora `HttpPeer::total_connection_timeout` + 应用层 deadline。
  - **stream_total_timeout_ms（流式）**：从 connect 完成到流式结束的总时间兜底，避免极慢上游或客户端不读导致连接永久挂住。应用层在 `response_body_filter` 内维护 deadline。

- **配置覆盖**：
  - `config.toml` 提供全局默认（FRD §15.2.2）：`timeout.connect_ms / read_ms / total_ms / stream_total_ms`
  - `entity:pricing:<model>` 提供 per-Model 覆盖
  - `entity:vendor:<vendor_id>.timeout_overrides` 提供 per-Vendor 覆盖（ADR-021）
  - 优先级（高 → 低）：Model > Vendor > 全局 config

- **触发逻辑（实现位置：`gw-proxy::ctx::Timeouts`）**：
  ```rust
  pub struct Timeouts {
      pub connect: Duration,
      pub read: Duration,
      pub total: Duration,           // 仅非流式生效
      pub stream_total: Duration,    // 仅流式生效
  }

  impl Timeouts {
      pub fn apply_to_peer(&self, peer: &mut HttpPeer, is_stream: bool) {
          peer.options.connection_timeout = Some(self.connect);
          peer.options.read_timeout = Some(self.read);   // 流式时即 idle timeout
          if !is_stream {
              peer.options.total_connection_timeout = Some(self.total);
          }
          // stream_total 在 response_body_filter 内自管 deadline
      }
  }
  ```

- **流式超时关闭语义（FRD §3.3）**：
  - 流式 idle timeout 触发时：
    1. 立即向客户端发送一个 `StreamEventIR::Error { code: GW_UPSTREAM_TIMEOUT, ... }` 事件（按客户端协议格式编码）
    2. 关闭流式连接（pingora 释放上游连接池占用）
    3. 已转发的 chunk 不可撤回，已收到 usage 按 ADR-004 失败矩阵"流式中断"路径结算（按已输出 token 扣费）
    4. **不重试**（ADR-022 流式不重试规则）
  - 流式 total timeout 触发时：同上语义。
  - 误差预算：触发时间误差 ≤ 500ms（FRD AC-3-3）。

- **非流式超时重试语义**：
  - `total_timeout_ms` 触发 → `RetryExecutor` 判断 `Instant::now() < retry_deadline` 时可重试（同 Key 或换 Key），由 `RetryConfig.total_budget_ms_nonstream`（默认 60s）决定总预算。
  - `connect_timeout_ms` 触发 → 重试时优先换 Key（已 visited 的 Key 在重试中跳过）。

- **实现位置（对齐 ADR-018）**：
  - `gw-core`：`TimeoutSpec` 类型定义
  - `gw-store`：从 `entity:pricing` / `entity:vendor` 解码 timeout overrides
  - `gw-proxy`：`Timeouts::apply_to_peer` + `response_body_filter` 内的 stream_total deadline 检查
  - `gw-routing`：`RetryExecutor` 消费 timeout 触发事件

- **业务规则**：
  - 客户端断开（`Cancelled`）不触发上述任何超时——pingora 检测到 client connection close 后立即 abort 上游（FRD §3.4），与超时路径独立。
  - 心跳（FRD §3.5）：上游带心跳（如 Anthropic `: ping`）→ 同协议透传时原样转发，跨协议时丢弃；客户端协议要求心跳但上游不发 → 网关每 15s 注入 keep-alive 注释行，防中间设备 idle 关闭——该注入**不重置** `read_timeout`（idle 检测的是真实数据 chunk）。

- **验收标准**：
  - AC-009-1：流式 idle 超时（30s 无 chunk）触发误差 ≤ 500ms（FRD AC-3-3）。
  - AC-009-2：非流式 total 超时触发后被 RetryExecutor 重试，2 次同 Key + 3 次跨 Key 失败后返回 `GW_UPSTREAM_TIMEOUT` 504。
  - AC-009-3：流式 total 兜底超时（600s）触发 → 发送 SSE error event + 关闭流，按已输出 token 计费。
  - AC-009-4：per-Model 配置 `total_timeout_ms = 300_000` 覆盖全局默认 60s，长 prompt 模型生效。

- **备选 A（已采用）**：4 类超时（connect / read-idle / total-nonstream / stream_total）。
- **备选 B（v2026-05-14）**：3 类（合并 total / stream_total 为单一 `total_timeout_ms`）。**否决**：流式合理总耗时（10 分钟）远大于非流式（1 分钟），合一会让非流式请求被错误地放大或流式被错误地缩短。
- **备选 C**：仅靠 pingora 内置的 connection_timeout / read_timeout，不在应用层管 stream_total。**否决**：pingora 不感知"流式 vs 非流式"语义，无法区分应用层 deadline。
- **未来重评信号**：
  - 出现"读 chunk 间隔正常但单 chunk 超大耗时极长"的极端场景 → 引入 `chunk_processing_timeout_ms` 第 5 类；
  - 心跳注入开始影响 idle 检测准确性（用户报告"实际无内容却没超时"）→ 改造 idle 计时器仅对"非心跳 chunk"重置。


### ADR-010 序列化 = sonic-rs 热路径，serde_json 其他（沿用）

- **决定**：`request_filter` 解析 body 抽 `model` 字段、`response_body_filter` 增量解析 SSE 都使用 `sonic-rs::get_from_str`（仅取目标字段、SIMD 加速）；非热路径（配置、事件载荷反序列化）用 serde_json。ProtocolCodec 实现内的字段级懒解析（ADR-018）也优先 `sonic-rs`。
- **备选**：simd-json。**否决**：mut 借用模型与 ProxyHttp 异步签名集成更繁琐。
- **未来重评信号**：sonic-rs 不再活跃维护时切换。

### ADR-011 Redis 客户端 = fred（沿用）

- **决定**：使用 `fred` crate；启用 `dynamic-pool`、`partial-tracing`、`metrics`、`subscriber-client` features。`subscriber-client` 用于 Stream 消费，`pool` 用于业务读写。
- **备选 A**：redis-rs + deadpool-redis。**否决**：Stream 消费 API 较啰嗦、Pub/Sub 与命令需独立连接，组合复杂度高。
- **备选 B**：rustis。**否决**：社区小、生产案例少。
- **未来重评信号**：fred 出现性能回归或维护停滞时切换。

### ADR-012 内存表 = 统一 Moka（沿用，已废弃 arc-swap + im 方案）

- **决定**：所有元数据资源（user / user_group / vendor / vendor_key / model_group / pricing / jwks / apikey）都用 `moka::future::Cache<K, Arc<Versioned<V>>>` 装，TTL + max_capacity 各自独立。读路径多一次 Moka 内部分片锁，相对于上游 RTT 可忽略。
- **配套**：高频写小状态（熔断器、本地扣账 `deducted` 计数、本地冻结 `frozen` 计数）继续用 `DashMap + AtomicXxx`。
- **备选 A**：`ArcSwap<Snapshot>` + `im::HashMap`（结构共享）。**否决**：与 ADR-005"完全懒加载"冲突——CoW 整快照适合"启动后基本静态"模型；改成懒加载意味着每次 miss-fill 都要 clone 整个 snapshot store 回去，写放大严重。统一 Moka 在懒加载场景天然契合。
- **备选 B**：DashMap。**否决**：缺乏 TTL / max_capacity / single-flight，需要自己实现冷数据淘汰。
- **备选 C**：parking_lot::RwLock<HashMap>。**否决**：写时全表阻塞读，破坏低延迟。
- **未来重评信号**：Moka 锁竞争出现在 profile 热点时，对低写入资源（如号池 model_group，秒级数十次更新）改回 ArcSwap+im 局部使用。

### ADR-013 通用交互协议（Entity / Counter / Append log / Alert）（沿用）

Rust ↔ Go ↔ Redis 之间的所有交互归约为以下协议族。新增业务只需新 namespace，**不**改协议结构。

#### 协议 1：Entity — 全量状态同步

```
# Redis
entity:<ns>:<id>             HASH    fields={ v: u64, enc: "json"|..., d: <bytes> }
                                     带 EXPIRE（TTL 由 namespace 决定，详见 ADR-016）
entity:<ns>:<id>:version     STRING  Go 上线后接管 INCR；当前阶段不使用

# 增量事件（Go 上线后启用；当前阶段 Rust 仅创建 consumer group 但流为空）
events:entity                STREAM  ns id v op enc d
                             op ∈ { upsert | delete }

# 冷加载链最末环 = PgSQL 直连（详见 ADR-016）
```

适用：读多写少、整体替换语义。例如号池整组（`model_group`、`vendor_key`）、用户元数据（`user`、`user_group`、`apikey`）、计价（`pricing`）、JWKS、Vendor。

**Writer 协调**：
- **当前阶段**：Rust 是 entity:* 的临时 writer（带 TTL 的 cache filler）。
- **Go 上线后**：Go 是权威 writer（无 TTL 的 SET HASH + INCR version），Rust 退化为短期影子 writer——只在 miss-fill 时回写带 TTL 的 HASH，`upsert_if_newer` 保证不会覆盖 Go 的较新版本（详见末尾"Writer 协调规则"）。

#### 协议 2：Counter — 单字段累加

```
# Redis
counter:<scope>:<id>:<field>   STRING (整数)，HINCRBY 或 INCRBY 累加

# 增量事件
events:counter                 STREAM  scope id field value ts_ms
                               value 是绝对值（携带快照），抗丢失、好对账

# 不需要兜底接口
miss = 0 是合法语义；冷启动直接 GET，miss 当作 0
```

适用：双 writer 累加场景。例如用户余额（`balance_micro`）、当日 token（`tokens_today_<YYYYMMDD>`）、累计 token（`tokens_total`）、Vendor Key 使用次数（`used_count`，FRD §9.6）。

#### 协议 3：Append log — 流水/事件记录

```
# Redis
<log_name>                    STREAM   appender XADD，consumer XREADGROUP
                              payload 自描述，由业务约定字段
```

适用：Rust → Go 单向流水。例如 `billing:records`、`alerts`。

#### 协议 4：Alert — Append log 的特化

```
# Redis
alerts                        STREAM   payload={ type, severity, ns?, id?, data }
```

Rust 写 / Go 读 / Go 转推送。区别于通用 append log 仅在于消费侧约定（Go 站做告警分发、Slack/邮件/SMS）。

- **决定**：以上 4 类协议覆盖**全部** Rust ↔ Go 交互；新业务必须归约到这些协议之一。
- **备选 A**：每业务自定义 schema（之前的方案）。**否决**：每加一种资源就要改 4 处代码（Redis schema、Go writer、Rust reader、stream dispatch），维护成本不可接受。
- **备选 B**：引入 RPC（gRPC/HTTP）双向调用。**否决**：增加运维复杂度（额外端口、证书、限流），且 Stream + 兜底 GET 已能满足"懒读 + 增量"语义。
- **未来重评信号**：未来出现"双向同步、需要 Rust 主动通知 Go"的场景（如 Rust 触发刷新某 entity），考虑加 RPC 协议或扩展 alerts 语义。

#### Writer 协调规则（Entity 协议）

Rust 与 Go 都可能写 `entity:<ns>:<id>`，靠两条规则避免冲突：

1. **TTL 区分**：Rust 写时**必须**附 `EXPIRE`（按 ADR-016 表）；Go 写时**禁止**附 EXPIRE（永久 key）。Redis 不区分"是否曾被 EXPIRE 过"，但语义上 Go 接管即等于"刷新成永久"。
2. **版本仲裁**：所有写都带 `v` 字段。读取/同步时使用 `upsert_if_newer`，严格大于才覆盖。Rust 写之前先 HGETALL 检查现有 v，若现有 v ≥ 待写 v 则放弃写入。

这两条规则保证：当 Go 上线写了 v=N 后，Rust 即使读到 PgSQL v=N-1（缓存击穿瞬间）也不会用旧值覆盖 Redis 中 Go 写的新值。

### ADR-014 Payload 编码可演进（沿用）

- **决定**：Entity 协议在 hash 中携带 `enc` 字段（默认 `json`），事件载荷同样带 `enc`；Rust 解码器按 `enc` 分派。当前实现仅 `json`，留出 `msgpack`、`zstd-json` 等扩展位。
- **备选 A**：固定 JSON。**否决**：未来若 entity 体积大到 KB 级（号池整组带 ≥ 100 key），切到 msgpack 可立省 30% IO，预留扩展点零成本。
- **备选 B**：固定 MessagePack。**否决**：可读性差、调试不便；Go 写 / Rust 读双方实现 MessagePack 也比 JSON 重。
- **未来重评信号**：实测 entity 体积持续超过 4KB 或 IO 成为 Stream 消费瓶颈时，逐 namespace 切换至 msgpack。

### ADR-015 余额 = Counter 协议 + 三层（base + deducted + frozen）（改写 / 取代 v2026-05-14 ADR-015）

- **决定（在 v2026-05-14 双层基础上新增 `frozen_micro`，配合 ADR-004 估算冻结模型）**：
  - 余额从 user entity 中**拆出来**，作为 Counter 协议（`counter:user:<uid>:balance_micro`）。两侧都只写 `HINCRBY` / `INCRBY`，**禁止 HSET 整字段**——天然消除"覆盖未结算扣账"风险。
  - 内存模型（三字段）：
    | 字段 | 含义 | 写入方 |
    |---|---|---|
    | `base_micro` | 最近一次 events:counter 同步的绝对值（Go 充值的快照） | Stream consumer |
    | `deducted_micro` | 本进程已结算但未 flush 的累计扣费 | logging phase |
    | `frozen_micro` | 本进程已冻结（in-flight 请求估算）未结算的累计金额 | request_filter / logging phase（解冻时减） |
  - 可用余额：`real = base_micro - deducted_micro - frozen_micro`。
  - `real ≤ 0` → 立即打开 `USER_CIRCUIT[uid]`（内存）+ 异步 `XADD alerts ...`（不阻塞响应）。
- **冻结生命周期**（与 ADR-004 协同）：
  - request_filter 通过校验 → `frozen_micro.fetch_add(estimate)` + 在 `ReqCtx` 中挂 `FreezeGuard`；
  - logging phase（无论成功/失败）→ `FreezeGuard::settle(actual_micro)`：原子地完成 `frozen_micro.fetch_sub(freeze_amount) + deducted_micro.fetch_add(actual_micro)`；
  - 异常路径（panic / 客户端断开 / pingora 提前 short-circuit / fail_to_proxy）→ `FreezeGuard::drop` 自动回滚 frozen（详细 RAII 机制见 ADR-004 "冻结释放的 RAII 保证"）；
  - **不变量**：进程任意时刻 `frozen_micro = Σ(in-flight 请求的 freeze_amount)`，请求结束时该项的贡献必为 0。
- **充值流程**：Go `INCRBY counter:user:<uid>:balance_micro <amount>` + `XADD events:counter * scope=user id=<uid> field=balance_micro value=<new_abs> ts_ms=<...>`。Rust 收到事件 → `base_micro = value`，**不**重置 `deducted_micro` / `frozen_micro`（事件 value 是 Go 视角的绝对值，未必反映本进程未 flush 的扣账与未结算冻结；让计费 worker 下一批 flush 自然把账平掉）。
- **断路解除**：充值事件到达且 `real > threshold` 后，下一次 user entity 访问时移除 `USER_CIRCUIT[uid]`；亦可在 counter 应用时直接尝试解除。
- **备选 A（已采用）**：三层（base + deducted + frozen）。
- **备选 B（v2026-05-14 ADR-015）**：双层（base + deducted），无 frozen。**否决**：FRD §10.3 要求估算冻结模型，必须把"已估未结"和"已结未 flush"分开记账，否则两个并发请求会重复消费同一份额度。
- **备选 C**：余额留在 user entity 的字段里。**否决**：违反"writer 唯一"——Go 充值与 Rust 扣账都要写同一 entity，HSET 时机错误就会冲掉对方未结算的写入；分离到 Counter 协议后语义彻底干净。
- **备选 D**：热路径同步 DECR Redis（绝对实时）。**否决**：与 ADR-004 同——2k+ QPS Redis 撑不住，且违反"热路径零 Redis IO"。
- **未来重评信号**：日终对账偏差长期超过万分之一，考虑给关键路径加 Lua 脚本原子事务（base / deducted / frozen 三字段在 Redis 端原子化）。

### ADR-016 冷加载链 = Rust → Redis → PgSQL（sqlx 直连）（沿用）

- **决定**：Rust 自己持有 PgSQL 连接池，Redis miss 时**直接 `SELECT`** 数据库，**不**走 Go HTTP。加载链：
  ```
  Moka.get_with(id, async {
      // L1: Redis Entity 协议
      if let Some(raw) = fred.hgetall(format!("entity:{ns}:{id}")).await? {
          return decode(raw);
      }
      // L2: PgSQL 直连
      let row = sqlx::query_as!(...).fetch_optional(&pg_pool).await?;
      let row = row.ok_or(NotFound)?;
      let bytes = encode_json(&row.payload);
      let v_new = row.version as u64;

      // L3: 写回 Redis（带 TTL，受 upsert_if_newer 保护）
      let pipe = fred.pipeline();
      pipe.hset(key, &[("v", v_new), ("enc", "json"), ("d", &bytes)]);
      pipe.expire(key, ttl_for(ns));
      pipe.flush().await.ok();   // 写失败仅 metrics，不阻塞响应

      Ok(Versioned { version: v_new, value: Arc::new(decode(&bytes)?) })
  })
  ```
- **驱动 / 连接路径**：
  - `sqlx` (features=`["postgres","runtime-tokio-rustls","macros","migrate"]`)，编译时 SQL 校验。
  - 直连 PgSQL（不走 PgBouncer）；每 pod 16 连接；statement timeout 50ms（cold path 单条主键查询）。
  - 离线 build 用 `sqlx prepare` 生成的 `.sqlx/` 缓存。
- **TTL 表**（每 namespace 差异化，新增 namespace 待 batch 2 §4 重整时统一定稿）：
  | Namespace | TTL |
  |---|---|
  | `user` / `user_group` / `apikey` | 5 min |
  | `model_group` / `vendor_key` / `vendor` | 30 min |
  | `pricing` | 30 min |
  | `jwks` | 1 h |
- **失败处理**：Redis miss + DB 不可达 → 503 `GW_GATEWAY_INTERNAL`（或更细的 backend_unavailable，详见 ADR-019 错误码字典）；缓存不污染。Rust 不写"不存在"占位，避免 negative caching 复杂度（DB 重连后下一请求自然恢复）。
- **性能预算**：
  - Redis HGETALL：< 1ms（同机房 RTT + 反序列化）
  - sqlx SELECT 主键：< 50ms（直连，p99 含连接获取）
  - 整个 cold-path 加载：< 60ms（含 Redis miss 探测 + DB 查询 + 回写）
- **备选 A（已采用）**：同 key + TTL + 版本仲裁。
- **备选 B**：独立 cache:* 命名空间。**否决**：双 key 路径让 Stream 事件应用要同时刷 cache:\* 与 Moka，复杂度上升；ADR-013 Writer 协调规则已能解决冲突。
- **备选 C**：不回写 Redis，仅填本地 Moka。**否决**：N 个 pod 各 miss 一次 = N 次 DB 查询，pod 数到几十时 cold path 压力线性放大；Redis 共享缓存是廉价收益。
- **备选 D（旧设计废案）**：Rust → Redis → Go HTTP `/internal/entity/:ns/:id`。**否决**：Go 服务在 cold path 上引入额外阻塞（一次 HTTP RTT + Go 内部 DB 查询），且现阶段 Go 还未设计；Rust 自己直连 DB 跳过中间层，延迟更稳定、依赖更少。
- **未来重评信号**：pod 数 > 50 导致 PgSQL 连接数压力大 → 引入 PgBouncer transaction-mode；多 pod 同时 cold-fill 同一 key 出现明显写抖动 → 加分布式 single-flight（Redis SETNX）。

### ADR-017 Vendor Key 加密存储 = AES-256-GCM + KMS（改写 / 取代 v2026-05-14 ADR-017）

> v2026-05-14 ADR-017 仅描述"最小列集契约"，未规定 `api_key` 的存储形式（隐含明文）。v2026-05-18 改写为 FRD §9.7 的加密存储语义；最小列集契约部分移至 §4 数据模型与 §8 PgSQL 契约章节单独维护。

- **决定**：
  - **算法**：AES-256-GCM（认证加密）。
  - **存储形式**：PgSQL 表 `vendor_keys` 中存放 `ciphertext: BYTEA NOT NULL`、`nonce: BYTEA NOT NULL`（96-bit IV，每条记录独立随机）、`auth_tag: BYTEA NOT NULL`（128-bit）、`kek_kid: TEXT NOT NULL`（KEK 标识，便于轮换）四列；**禁止**任何列存放明文 `api_key`。
  - **KEK 来源**：
    - 生产：外部 KMS（AWS KMS / GCP KMS / HashiCorp Vault），通过 KMS API 在进程启动时拉取，缓存于内存（`secrecy::SecretVec<u8>`）。
    - 开发 / 单测：环境变量 `GW_KMS__KEK_HEX=<64 hex chars>`（明确仅 dev/test）。
    - KEK 永不入 PgSQL、永不入 Redis、永不入日志。
  - **解密时机**：
    - cold-path 加载 `entity:vendor_key:<id>` 时（ADR-016 加载链 L2 PgSQL SELECT 之后、写回 Redis 之前），在内存中解密为 `KeyEntry { api_key: SecretString, ... }`。
    - 写回 Redis 的 `entity:vendor_key:<id>` HASH **存放密文**（同样的 ciphertext + nonce + auth_tag + kek_kid 四字段），Rust 内存里的 `SecretString` 仅 ad-hoc 解密一次，缓存在 Moka 中的 `Arc<Versioned<KeyEntry>>` 持有 `SecretString` 形式。
    - **Redis 不存明文**：即便 Redis 被嗅探，攻击者拿到的也是同样的密文 + nonce；KEK 在 Rust 进程内存中。
  - **使用时机**：
    - `upstream_request_filter` 阶段：从 `ReqCtx.picked_key.api_key` 取出 `SecretString::expose_secret()`，按 Vendor.auth_template 拼到 Authorization 头，写入 pingora 的 `RequestHeader` 后**立刻 drop 字符串引用**（不进任何 local var、不打日志、不存 metric label）。
    - 解密后的明文 `api_key` 在内存中以 `secrecy::SecretString` 持有，`Debug` / `Display` 自动 `[REDACTED]`，序列化时也被屏蔽。
  - **展示侧 mask**：网关不提供 admin API，但管理面（Go）从 PgSQL 解密后展示时必须遵循 FRD §9.7"前 4 位 + 后 4 位"格式（如 `sk-1a**3f`）。该约束写入 PgSQL 契约文档而非 spec ADR。
- **KEK 轮换**：
  - 新 KEK 上线时：管理面读全部 ciphertext → 用旧 KEK 解 → 用新 KEK 重新加密 → 写回 PgSQL（含新 `kek_kid`）。
  - 双 KEK 期：网关启动时同时持有 `kek_kid → key` 映射，按记录的 `kek_kid` 选择解密 KEK。
  - 完成轮换后软删除旧 KEK 在内存中的副本；记录 audit log。
- **失败处理**：
  - 解密失败（auth_tag mismatch / ciphertext 损坏 / KEK 缺失）→ 该 Vendor Key 立即标记为不可用（不进入 Moka 缓存）+ `XADD alerts severity=critical type=key_decrypt_failed kid=<key_id>`；该 model_group 的路由会自然降级到其他 Key（FRD §13.1 单 Vendor Key 故障）。
  - KMS 不可达 → 进程启动失败（fail-fast，不允许"半破"运行）；运行期 KMS 短暂不可达不影响已缓存的解密结果（KEK 已在内存）。
- **安全约束**：
  - `api_key` 明文严禁出现在：tracing 日志、metrics label、错误响应体、debug print、panic backtrace；
  - 通过 `secrecy::SecretString` 类型系统强制（`Debug` impl 已被屏蔽）；
  - PR review checklist 必须有"是否新增了 SecretString 的 expose_secret 调用"一项。
- **备选 A（已采用）**：AES-256-GCM + 外部 KMS。
- **备选 B（v2026-05-14 隐含）**：明文存储 `api_key`，仅靠 PgSQL/Redis 自身的访问控制保护。**否决**：FRD §9.7 / §11 明确要求字段级加密；明文存储一旦 DB 备份外泄即等于全部 Key 泄漏，与"密钥级别防护"语义不符。
- **备选 C**：在 PgSQL 端用 `pgcrypto` 透明加密（pg-side 加密 + key in DB）。**否决**：KEK 与密文同库存放，DB 备份外泄时同时拿到，等价于明文存放；不符合 KEK / DEK 分离原则。
- **备选 D**：对称密钥 ChaCha20-Poly1305 替代 AES-GCM。**未来可选**——FRD 未指定具体算法，AES-256-GCM 是云 KMS 标准支持算法；如果硬件不支持 AES-NI（ARM Cortex-A53 等），ChaCha20-Poly1305 性能更好。重评信号 = 部署到非 x86 / 非 Apple Silicon 环境。
- **备选 E**：包封 envelope encryption（KMS 包封 DEK，DEK 加密 api_key，DEK 与 ciphertext 一起存）。**未来可选**——单 Key 数据量小（< 256B）不显著受益于 envelope；当 vendor_keys 表数据量超过 KMS API 限流阈值时再演进。
- **未来重评信号**：
  - 合规要求审计每次解密（FIPS 140-3、ISO 27001 严苛场景）→ 切换为 envelope encryption + KMS 全程参与；
  - 性能 profile 显示 AES-GCM 占 CPU > 5% → 评估 ChaCha20-Poly1305；
  - pod 重启频率上升（如蓝绿部署或 spot 实例频繁回收）导致 KMS `Decrypt` API 调用接近账户配额 → 评估 envelope encryption（KMS 仅参与 KEK 解封，业务热路径用进程内的 DEK 解密），或在 KMS 与 Rust 之间加一层 sidecar 缓存（如 Vault Agent）。

### ADR-018 Crate 分层规则（新增）

> 本 ADR 是后续所有"它的实现放哪？"问题的总纲。所有新增 ADR / 新增模块都必须按本 ADR 定义的 9-crate DAG 自报位置。

- **决定**：v1 创建 9 个 crate，按以下 DAG 组织。每个 crate 的存在理由 = "隔离一类关注点，让设计更易理解"，或"Rust 语言强约束（proc-macro = true 必须独立 crate）"。

  ```
                    gw-gateway (bin)
                          │
                          ▼
                      gw-proxy
                  ┌───────┼───────┬───────┐
                  ▼       ▼       ▼       ▼
              gw-codec  gw-auth  gw-routing  gw-ledger
                  │       │       │            │
                  ▼       ▼       ▼            │
          gw-codec-macros gw-store ◄───────────┘
          (proc-macro)        │
                  │           ▼
                  └─────► gw-core (类型 / IR / ErrorIR / Versioned / ID newtype / KeyStatus)
  ```

- **9 个 crate 与各自关注点**：

  | Crate | 关注点 | 暴露的核心 trait / 类型 | 依赖 |
  |---|---|---|---|
  | `gw-core` | 纯类型层：IR、ErrorIR、Versioned、ID newtype（UserId、KeyId、ModelName、Group）、Encoding enum、KeyStatus 7-state enum、ErrorCategory enum、SecretString re-export | `RequestIR<'a>` / `ResponseIR<'a>` / `StreamEventIR<'a>` / `ErrorIR<'a>` / `Versioned<T>` / `KeyStatus` / `ErrorCategory` | （无业务依赖；仅依赖 `bytes` / `serde` / `secrecy` 等基础 crate） |
  | `gw-codec-macros` | **proc-macro crate**（`proc-macro = true`）：`#[derive(IrConvert)]` 等派生宏，编译期为外部协议结构体生成 `From<X> for RequestIR` / `TryFrom<&RequestIR> for X` 等转换代码，全部 `#[inline(always)]`。**不是为分层而拆，是 Rust 语言强约束**——proc-macro crate 无法与普通 lib code 共存 | `#[derive(IrConvert)]` / `#[derive(IrStreamConvert)]` / `#[derive(IrErrorConvert)]` | `proc-macro2` / `syn` / `quote` |
  | `gw-codec` | 协议双向 codec：OpenAI / Anthropic / Gemini 三实现（同 crate 内 mod），每个 mod 定义外部结构体 + 用 derive 宏生成转换 + 手写 `ProtocolCodec` trait 包装；外部结构体通过 derive 自动得到 IR 转换；`AnyCodec` enum 分派 | `ProtocolCodec` trait + 3 个 impl struct + `AnyCodec` enum | `gw-core` + `gw-codec-macros` |
  | `gw-store` | Redis (fred) 客户端 + PgSQL (sqlx) 客户端 + Entity 协议 Moka 缓存 + Counter 协议 + DeductionState（三层余额）+ 冷加载链（ADR-016）+ Stream consumer（ADR-007） | `EntityStore` / `CounterStore` / `Entity` trait / `DeductionState` | `gw-core` |
  | `gw-auth` | JWT 验签 + APIKey 校验 + 五级权限合并算法（FRD §6.2）+ 单请求 hard cap 校验 + IP 黑白名单 + 签名 + 限流 | `Authenticator` / `MergedPermissions` / `RateLimitGuard` | `gw-core` + `gw-store` |
  | `gw-routing` | Strategy trait + 8 实现 + 熔断器（ADR-008）+ 重试执行器（ADR-022）+ Fallback 叠加层 | `Strategy` trait / `BreakerState` / `RetryExecutor` | `gw-core` + `gw-store` |
  | `gw-ledger` | 计费 mpsc ingest + 批量 flush worker + BillingRecord schema + FreezeGuard RAII + alerts XADD | `Ledger` trait / `BillingRecord` / `FreezeGuard` | `gw-core` + `gw-store` |
  | `gw-proxy` | ProxyHttp impl + 所有 filter + ReqCtx + Pingora 集成（HttpPeer / set_http_version / set_timeout 等）+ DetailedLog writer | `LlmGateway: ProxyHttp` | 以上全部 |
  | `gw-gateway` | bin：main、config（figment）、tracing 初始化、Pingora server 启动、KMS 初始化、Pingora HTTP listener 装配 | `fn main` | `gw-proxy` |

- **依赖方向硬约束**：
  1. **禁止反向依赖**：低层 crate 永远不能 `use` 高层 crate（例如 `gw-core` 不能 `use gw-store`）。
  2. **禁止兄弟互依**：同层 crate 互不依赖（`gw-auth` 不依赖 `gw-routing`，`gw-codec` 不依赖 `gw-auth`）。共享数据通过 `gw-core` 中转，共享 IO 能力通过 `gw-store` 中转。
  3. **proc-macro 单向依赖**：`gw-codec-macros` **仅被** `gw-codec` 依赖（作为 build-dependencies / dependencies 中的 `proc-macro = true` crate）；它本身不能依赖 `gw-core`（proc-macro crate 在编译宿主 crate 时执行，不可携带运行时类型），生成的代码通过完全限定路径引用 `gw_core::*` 类型。
  4. **禁止循环依赖**：编译期 cargo check 验证。

- **可拼装 / 替换实现**：
  - `ProtocolCodec` / `Strategy` / `Ledger` / `Entity` 都是 trait；`gw-proxy` 通过泛型参数接收实现，编译期单态化。
  - 替换实现 = 在同 crate 内新增一个 mod + impl，不需要新建 crate。
  - 新增第 4 个协议（如 Cohere）= 在 `gw-codec` 新增 `mod cohere`：(a) 定义 `CohereRequest` / `CohereResponse` 等外部结构体 + `#[derive(IrConvert)]`；(b) 实现 `ProtocolCodec` trait（trait 方法体调用 derive 生成的 from/into）。不动其他 crate，也不动 `gw-codec-macros`（除非要扩展 IR 字段语义）。

- **"可拼装"的语义边界（与 ADR-002 一致）**：
  - "可拼装" = **编译期**可拼装，不是运行时可拼装。`LlmGateway<C: ClientCodec, U: UpstreamCodec, S: Strategy, L: Ledger>` 是单态化绑定的；一旦 main 启动选定了具体类型组合，运行时无法切换实现。
  - 不存在"热加载 Strategy 实现"或"按请求选 Codec impl"的场景（FRD §15 + ADR-002 已明确"策略变更走重新部署"）。
  - **同 trait 多实现的运行时分派**（如 `ClientCodec` 要根据请求路径分派到 OpenAI / Anthropic / Gemini）由 enum + match 实现，而非 trait object：
    ```rust
    pub enum AnyCodec {
        OpenAi(openai::OpenAiCodec),
        Anthropic(anthropic::AnthropicCodec),
        Gemini(gemini::GeminiCodec),
    }
    impl AnyCodec {
        #[inline] pub fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, _> {
            match self {
                AnyCodec::OpenAi(c) => c.parse_request(body),
                AnyCodec::Anthropic(c) => c.parse_request(body),
                AnyCodec::Gemini(c) => c.parse_request(body),
            }
        }
    }
    ```
  - 这样 `gw-proxy` 接收 `AnyCodec` 一个具体类型，三种 codec 共存于二进制，路由阶段按 detect 结果选 enum 变体；保持单态化 + inline 友好，避免 `Box<dyn ClientCodec>`。

- **trait 与多实现共置原则**：
  - 多个并列实现（≥ 2 个）放同一 crate 的 mod 下，靠 enum/泛型分发；
  - 拆成多 crate 的代价 = trait object 间接调用 + 跨 crate 阻碍 inline + 额外 Arc 引用计数；收益 = 几乎为零（同一团队维护、同一发布节奏）。
  - **唯一例外**：`gw-codec-macros` 必须独立成 crate，因为 Rust 要求 `proc-macro = true` 的 crate 不能与普通 lib code 共存。这是语言约束、非分层选择。

- **v1 不创建的 crate（FRD 标 P2 或边缘能力）**：
  - `tokenizer-estimator`（FRD §14，P2）— 预留接入点：`gw-core` 中定义 `pub trait TokenEstimator`，`gw-ledger` 的计费回退路径调用此 trait；v1 实现 = `gw-ledger` 内一个 `NoopEstimator` impl（返回保守上限）。**不为它建 crate**。
  - `content-audit` / 敏感词（FRD §11.3-11.4，P1 但属边缘安全）— 预留接入点：`gw-proxy` 的 request/response body filter 内留 `pub trait ContentFilter` 调用点；v1 实现 = 不接入 ContentFilter（filter 槽为空）。**不为它建 crate**。
  - `shadow-mirror` 影子流量（FRD §7.8，P2）— **不实现**。
  - `sticky-routing` 粘性路由（FRD §7.7，P2）— 作为 `gw-routing` 内的一个 `Strategy` impl 槽位预留。**不为它建 crate**。
  - 任何 FRD 中标 P2 / V2 演进的能力 — spec 中只在主流程描述扩展点（trait + feature gate），不在 §6 列 crate。

- **跨 crate 调用的零成本前提**：
  - 跨 crate 调用在 Rust 中是零成本抽象（保证条件：泛型单态化 + `#[inline]` + LTO）；
  - release build 必须开启 `lto = "thin"`（最坏 `"fat"`）以保证跨 crate inline 发生；
  - 热路径 trait 方法（`parse_request` / `render_request` / `parse_stream_chunk` / `Strategy::pick`）必须标 `#[inline]`；
  - **derive 宏生成的所有 `From` / `TryFrom` 方法体内全部 `#[inline(always)]`**（在 `gw-codec-macros` 的代码生成模板中固化）；
  - 验证手段：`cargo asm` 检查 OpenAI→OpenAI 路径在 LTO 后是否还存在 `serde_json::Value::get` / `Box::new` / `core::fmt::Display::fmt` 等可消除调用；`cargo expand` 检查 derive 宏展开后是否为直接字段赋值。

- **新增 crate 的四问校验**（提议任何新 crate 前必须回答）：
  - (a) 它属于 DAG 哪一层？
  - (b) 它依赖哪些更低层 crate？（保证不形成环 / 不跨层）
  - (c) v1 是否非有它不可？（边缘能力一律答否；语言强约束如 proc-macro 自动答是）
  - (d) 拆出来会不会引入新概念 / 新生命周期 / 新转换层？（如有则倾向合并；语言强约束的例外可豁免）
  任一不通过 → 不要这个 crate，挪到现有 crate 的 mod。

- **备选 A（已采用）**：9 crate（粒度 = 关注点 + 1 个 proc-macro 强约束）。
- **备选 B**：单 crate（mono-crate）。**否决**：FRD §2.3 / §7.3 要求 codec / strategy 可拼装替换；mono-crate 让单测和复用变难。
- **备选 C**：14+ crate（按 FRD 章节直接映射，每章一个 crate）。**否决**：见反馈 — codec 三实现拆三个 crate 强制 dyn dispatch；权限合并、单请求 hard cap 拆出独立 crate 等同于 `gw-auth` 内一个函数 → 没有简化设计，反而引入新概念。
- **备选 D**：把 `gw-routing` 拆成 `gw-policy` + `gw-breaker` + `gw-retry`。**否决**：三者强耦合（Strategy.pick 必须读 BreakerState，RetryExecutor 必须读 Strategy 和 BreakerState），拆开后大量 Arc 反复传递且引入新转换 trait；合并为 `gw-routing` 是简化设计。
- **备选 E**：把 `gw-store` 拆成 `gw-store-redis` + `gw-store-pgsql` + `gw-store-cache`。**否决**：v1 阶段没有"只用 redis 不用 pgsql"的场景，三者都被 ADR-016 加载链绑定；拆开后 ADR-016 加载链横跨三 crate，复杂度上升。
- **备选 F**：不用 proc-macro，全手写 codec 字段映射。**否决**：FRD AC-2.3-1 要求同协议透传 P50 ≤ 200µs；手写映射在 hot path 仍有 `sonic-rs::get_from_str` 的字符串匹配开销（虽 SIMD 加速但不为零），且新增协议代码量大、易错。derive 宏让"字段映射"在编译期完全固化为直接字段赋值，零运行时查找开销，新增协议代码量减半。

- **未来重评信号**：
  - 单个 crate 超过 10K 行 LoC（暗示关注点已混入）→ 评估拆分；
  - 出现"我需要为这个能力用 cargo feature 切换两种实现"的真实场景（不是预设）→ 评估拆分该 trait 与多实现；
  - 编译时间出现明显恶化（增量 build > 30s）→ 评估按 crate 边界做更细切分（但优先用 cargo workspace 并行 + sccache）；
  - proc-macro 展开后代码膨胀严重（二进制 size > 100MB）→ 评估让 macro 生成更紧凑的代码（如用 generic helpers 替代 inline-everything）。


### ADR-019 协议层 IR + ProtocolCodec trait（新增）

> **驱动需求**：FRD §2（协议层）+ AC-2.3-1 / AC-2.3-2。解决"N 个客户端协议 × M 个上游协议 = N×M 对转换"的组合爆炸为 (N+M) 个 trait 实现。

- **决定**：
  - 在 `gw-core` 定义统一中间表示 IR（强类型 Rust struct，**非** `serde_json::Value`）：
    ```rust
    pub struct RequestIR<'a> {
        pub model: Cow<'a, str>,
        pub messages: Vec<MessageIR<'a>>,
        pub stream: bool,
        pub max_tokens: Option<u32>,
        pub temperature: Option<f32>,
        pub top_p: Option<f32>,
        pub top_k: Option<u32>,
        pub presence_penalty: Option<f32>,
        pub frequency_penalty: Option<f32>,
        pub stop: Option<Vec<Cow<'a, str>>>,
        pub seed: Option<u64>,
        pub tools: Option<Vec<ToolIR<'a>>>,
        pub tool_choice: Option<ToolChoiceIR<'a>>,
        pub response_format: Option<ResponseFormatIR>,
        pub cache_control: Option<CacheControlIR>,
        pub metadata: MetadataIR<'a>,                  // 厂商专属字段保留区
        pub extensions: Option<HashMap<&'a str, &'a RawValue>>, // 未识别字段原样保留
        untouched: UntouchedFlags,                     // 零成本透传标志（见下文）
    }
    pub enum MessageIR<'a> {
        System { content: ContentIR<'a> },
        User { content: ContentIR<'a> },
        Assistant { content: ContentIR<'a>, tool_calls: Option<Vec<ToolCallIR<'a>>> },
        Tool { tool_call_id: Cow<'a, str>, content: ContentIR<'a> },
    }
    pub enum ContentIR<'a> {
        Text(Cow<'a, str>),
        Parts(Vec<ContentPart<'a>>),                  // 多模态
    }
    pub enum ContentPart<'a> {
        Text(Cow<'a, str>),
        Image { source: ImageSource<'a> },
        Audio { source: MediaSource<'a> },
        File  { source: MediaSource<'a> },
    }
    pub enum ImageSource<'a> { Url(Cow<'a, str>), B64 { media_type: Cow<'a, str>, data: Cow<'a, str> } }

    pub struct ResponseIR<'a> {
        pub id: Cow<'a, str>,
        pub model: Cow<'a, str>,
        pub messages: Vec<MessageIR<'a>>,
        pub finish_reason: Option<FinishReasonIR>,
        pub usage: Option<UsageIR>,
        pub metadata: MetadataIR<'a>,
    }
    pub struct UsageIR { pub input_tokens: u32, pub output_tokens: u32, pub cached_tokens: Option<u32> }

    pub enum StreamEventIR<'a> {
        Start { request_id: Cow<'a, str> },
        Delta { text: Option<Cow<'a, str>>, tool_call: Option<ToolCallIR<'a>>, thinking: Option<Cow<'a, str>> },
        Usage(UsageIR),
        End { finish_reason: FinishReasonIR },
        Error(ErrorIR<'a>),                            // 见 ADR-020
    }
    ```
  - **在 `gw-core` 定义 `ProtocolCodec` trait**：
    ```rust
    pub trait ProtocolCodec {
        type Error: Into<ErrorIR<'static>>;

        fn detect_request(headers: &HeaderMap, body_head: &[u8]) -> Option<Self> where Self: Sized;

        #[inline] fn parse_request<'a>(&self, body: &'a [u8]) -> Result<RequestIR<'a>, Self::Error>;
        #[inline] fn render_request(&self, ir: &RequestIR<'_>, buf: &mut BytesMut) -> Result<(), Self::Error>;

        #[inline] fn parse_stream_chunk<'a>(&self, chunk: &'a [u8]) -> Result<Option<StreamEventIR<'a>>, Self::Error>;
        #[inline] fn render_stream_chunk(&self, ir: &StreamEventIR<'_>, buf: &mut BytesMut) -> Result<(), Self::Error>;

        #[inline] fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, Self::Error>;
        #[inline] fn render_response(&self, ir: &ResponseIR<'_>, buf: &mut BytesMut) -> Result<(), Self::Error>;

        /// 上游错误体 → ErrorIR（见 ADR-020）
        #[inline] fn parse_error<'a>(&self, status: u16, body: &'a [u8]) -> ErrorIR<'a>;
        /// ErrorIR → 客户端协议错误体（见 ADR-020）
        #[inline] fn render_error(&self, err: &ErrorIR<'_>, buf: &mut BytesMut);

        /// 输入 token 数估算（FRD §5.6 + §14），用于 max_input_tokens 校验与计费回退
        #[inline] fn estimate_input_tokens(&self, ir: &RequestIR<'_>) -> u32;
    }
    ```
  - **协议实现 = 外部结构体 + `#[derive(IrConvert)]` 派生宏 + 手写 trait 包装**（共置 `gw-codec` crate，对齐 ADR-018）：
    - 每个协议在 `mod openai` / `mod anthropic` / `mod gemini` 内定义自己的 wire-format struct（`OpenAiRequest<'a>` 等），用 `#[derive(IrConvert)]` 自动生成与 IR 的双向转换；
    - `pub struct OpenAi;` / `Anthropic;` / `Gemini;` 实现 `ProtocolCodec` trait，trait 方法体调用 derive 生成的 `From` / `TryFrom`，**不手写字段映射**；
    - `pub enum AnyCodec { OpenAi, Anthropic, Gemini }` 作为运行时分派的统一入口（详见 ADR-018 "可拼装语义边界"）。

- **derive 宏驱动的字段映射（核心：编译期固化，零运行时查找）**：

  proc-macro 提供四个派生宏（实现在 `gw-codec-macros` crate）：

  | 宏 | 作用 | 生成的 impl |
  |---|---|---|
  | `#[derive(IrConvert)]` | 请求 / 响应顶层结构 ↔ `RequestIR` / `ResponseIR` | `From<X<'a>> for RequestIR<'a>` + `TryFrom<&RequestIR<'_>> for X<'a>` |
  | `#[derive(IrStreamConvert)]` | 流式 chunk ↔ `StreamEventIR` | `From<X<'a>> for StreamEventIR<'a>` + 反向 |
  | `#[derive(IrErrorConvert)]` | 上游错误体 ↔ `ErrorIR` | 同上 |
  | `#[derive(DetectRequest)]` | 协议识别 | `fn detect(headers, body_head) -> Option<Self>` |

  使用示例（`mod openai` 内）：
  ```rust
  use gw_codec_macros::IrConvert;
  use gw_core::ir::*;

  #[derive(IrConvert, Deserialize, Serialize)]
  #[ir(target = "RequestIR", direction = "both")]
  pub struct OpenAiRequest<'a> {
      #[ir(map = "model")]
      pub model: Cow<'a, str>,

      #[ir(map = "messages", via = "openai_msg_vec")]
      pub messages: Vec<OpenAiMessage<'a>>,

      #[ir(map = "stream", default = "false")]
      pub stream: bool,

      #[ir(map = "max_tokens")]
      pub max_tokens: Option<u32>,

      #[ir(map = "temperature")]
      pub temperature: Option<f32>,

      // OpenAI 顶层 messages 中可能含 system 角色——via 函数处理拆分
      // OpenAI 没有独立 system 字段，但 Anthropic 有 → IR `system` 来源由 messages 的 System 变体提供
      #[ir(map = "tools", via = "openai_tools")]
      #[serde(skip_serializing_if = "Option::is_none")]
      pub tools: Option<Vec<OpenAiTool<'a>>>,

      #[ir(map = "tool_choice", via = "openai_tool_choice")]
      #[serde(skip_serializing_if = "Option::is_none")]
      pub tool_choice: Option<OpenAiToolChoice<'a>>,

      // OpenAI 特有 → IR.metadata.openai_logit_bias
      #[ir(map = "metadata.openai_logit_bias")]
      #[serde(skip_serializing_if = "Option::is_none")]
      pub logit_bias: Option<HashMap<Cow<'a, str>, f32>>,

      // 未识别字段 → IR.extensions
      #[ir(extensions)]
      #[serde(flatten)]
      pub extra: HashMap<&'a str, &'a RawValue>,
  }
  ```

  宏展开后（`cargo expand` 可见）：
  ```rust
  impl<'a> From<OpenAiRequest<'a>> for RequestIR<'a> {
      #[inline(always)]
      fn from(src: OpenAiRequest<'a>) -> Self {
          RequestIR {
              model: src.model,                                          // Cow 直接 move
              messages: openai_msg_vec(src.messages),                    // via 函数：Vec<OpenAiMessage> → Vec<MessageIR>
              stream: src.stream,
              max_tokens: src.max_tokens,
              temperature: src.temperature,
              top_p: None, top_k: None, presence_penalty: None,
              frequency_penalty: None, stop: None, seed: None,           // 该协议不存在的字段
              tools: src.tools.map(openai_tools),
              tool_choice: src.tool_choice.map(openai_tool_choice),
              response_format: None, cache_control: None,
              metadata: MetadataIR {
                  openai_logit_bias: src.logit_bias,
                  anthropic_thinking: None,
                  gemini_safety_settings: None,
                  ..Default::default()
              },
              extensions: if src.extra.is_empty() { None } else { Some(src.extra) },
              untouched: UntouchedFlags::ALL_UNTOUCHED,
          }
      }
  }

  impl<'a> TryFrom<&RequestIR<'a>> for OpenAiRequest<'a> {
      type Error = IrConvertError;
      #[inline(always)]
      fn try_from(ir: &RequestIR<'a>) -> Result<Self, Self::Error> {
          Ok(OpenAiRequest {
              model: ir.model.clone(),                                   // Cow 复制（Borrowed 仍是借用）
              messages: openai_msg_vec_rev(&ir.messages)?,
              stream: ir.stream,
              max_tokens: ir.max_tokens,
              temperature: ir.temperature,
              tools: ir.tools.as_ref().map(openai_tools_rev).transpose()?,
              tool_choice: ir.tool_choice.as_ref().map(openai_tool_choice_rev).transpose()?,
              logit_bias: ir.metadata.openai_logit_bias.clone(),
              extra: ir.extensions.clone().unwrap_or_default(),
          })
      }
  }
  ```

  - **via 函数**用于复合类型（如 `Vec<Message>`、`Tool`、`ContentPart`）的转换，开发者在 `mod openai` 内手写一次，宏只负责"在顶层字段上调用"。via 函数本身也标 `#[inline(always)]`。
  - **缺失字段**（IR 有但该协议没有，如 OpenAI 没有 Anthropic 的 `thinking`）→ 宏生成 `None` / 默认值（通过 `#[ir(map = "...")]` 的存在性决定，缺失即 `Default::default()`）；
  - **协议特有字段**（如 OpenAI 的 `logit_bias`）→ 通过 `#[ir(map = "metadata.openai_logit_bias")]` 写入 IR 的扁平 `MetadataIR` 子字段；
  - **未识别字段**（`#[ir(extensions)]` + `#[serde(flatten)]`）→ 自动归入 `RequestIR.extensions`，保证协议升级时不丢字段。

- **零运行时开销的保证**：
  - 宏展开后**没有任何字符串字段名查找**（无 `Map::get("model")`、无 `serde_json::Value::pointer`）；
  - 所有字段映射是直接的 struct 字段访问与赋值；
  - `via` 函数对复合类型递归，每一步都 `#[inline(always)]`，LLVM 在 LTO 时可全链路展开；
  - `Cow<'a, str>` 在同协议同生命周期场景下保持 `Cow::Borrowed`，不触发堆分配；
  - 跨协议路径上的 mutation（如改 `max_tokens`）→ 仅触动该字段，其他字段保持 `Cow::Borrowed` 状态。

- **零成本同协议透传的实现（FRD §2.3 / AC-2.3-1）**：
  - 同协议路径（如 OpenAI→OpenAI）由 `gw-codec::passthrough::same_protocol_passthrough<C>` 泛型函数实现：
    ```rust
    #[inline(always)]
    pub fn same_protocol_passthrough<'a, C: ProtocolCodec>(
        codec: &C,
        body: &'a [u8],
        mutations: &Mutations,
    ) -> Result<Cow<'a, [u8]>, C::Error> {
        if mutations.is_empty() {
            return Ok(Cow::Borrowed(body));                  // 字节级直转，零拷贝
        }
        // 有 mutation 时才走 derive 宏生成的 try_from + render（仍 inline）
        let mut ir = codec.parse_request(body)?;
        mutations.apply_to(&mut ir);
        let mut buf = BytesMut::with_capacity(body.len());
        codec.render_request(&ir, &mut buf)?;
        Ok(Cow::Owned(buf.to_vec()))
    }
    ```
  - `Mutations` 收集 request_filter / upstream_request_filter 阶段产生的字段改动（`max_tokens` cap、`metadata` 注入、`tool_choice` 翻译）；空 mutation 直接返回原 body。
  - **验证手段**：`cargo asm` 检查 OpenAI→OpenAI 无 mutation 路径在 LTO 后无 `serde_json::ser` / `Vec::push` 等堆操作；基准 P99 ≤ 800µs（payload 8KB）。

- **跨协议路径（FRD AC-2.3-2）**：
  - `forward<C: ProtocolCodec, U: ProtocolCodec>` 泛型函数，编译期单态化每对 `(C, U)`：
    ```rust
    let client_struct: OpenAiRequest = sonic_rs::from_slice(body)?;     // 一次 deserialize
    let ir: RequestIR = client_struct.into();                            // derive 宏生成，#[inline(always)]
    apply_mutations(&mut ir, &ctx);                                       // 业务层 mutation
    let upstream_struct: AnthropicRequest = (&ir).try_into()?;            // derive 宏生成
    sonic_rs::to_writer(&mut buf, &upstream_struct)?;                    // 一次 serialize
    ```
  - 预算 P99 ≤ 2ms（含上述 mutation，不含上游 RTT）。
  - **关键点**：从外部 wire-format 到 IR 的转换是**编译期固化的字段移动**，不是运行时的 JSON 字段名匹配。运行时只剩两次必要的 sonic-rs serialize/deserialize（这是 wire format 与 Rust struct 之间的桥梁，无法消除）。

- **协议特性映射要点（FRD §2.5 摘录）**：
  - **角色与对话结构**：OpenAI 顶层 `messages[].role` / Anthropic 独立 `system` 字段 / Gemini `systemInstruction`。各自的 wire-struct 各自定义；via 函数（`openai_msg_vec` / `anthropic_msg_vec`）负责把外部 messages 转为 `Vec<MessageIR>`，IR 内部统一 `MessageIR::System`。
  - **工具调用**：OpenAI `tools[].function` + `message.tool_calls[]` / Anthropic `tool_use` content block / Gemini `functionDeclarations` + `functionCall` part。IR 统一为 `ToolIR { name, description, parameters_jsonschema }`、`ToolCallIR { id, name, arguments_json }`。各协议的 via 函数把自家结构映射到 IR。
  - **流式 delta 事件**：OpenAI `data: ... [DONE]` / Anthropic 多种 `event:` + `data:` 行 / Gemini chunked JSON 对象。各 codec 定义自己的 chunk wire-struct + `#[derive(IrStreamConvert)]`；终止符识别在 `parse_stream_chunk` 的手写包装层。
  - **多模态**：OpenAI `image_url` / Anthropic `image.source.{type, media_type, data}` / Gemini `inlineData` 或 `fileData`。各协议的 `ContentPart` wire-struct + via 函数映射到 IR `ContentPart::Image { source: ImageSource }`。
  - **厂商专属特性保留**：Anthropic `thinking` → `MetadataIR::anthropic_thinking`；OpenAI `logprobs` / `logit_bias` → `MetadataIR::openai_logprobs` / `openai_logit_bias`；Gemini `safetySettings` → `MetadataIR::gemini_safety_settings`。目标协议的 wire-struct 上对应字段标 `#[serde(skip_serializing_if = "Option::is_none")]`，自动按"丢弃 + debug 日志"处理（不报错）。

- **协议识别（FRD §2.4）**：
  - 客户端协议由 `detect_request(headers, body_head)` 联合识别：路径前缀（`/v1/chat/completions` / `/v1/messages` / `/v1beta/models/`）+ 关键头 + body 前 N 字节 sonic-rs 检测。
  - 实现：每个 codec 用 `#[derive(DetectRequest)]` + `#[detect(path_prefix = "/v1/chat/completions", header_contains = "...")]` 属性，宏生成 `fn detect(...) -> Option<Self>`。
  - 识别失败返回 400 `GW_REQUEST_PROTOCOL_UNKNOWN`（详见 ADR-020 错误码字典）。
  - 上游协议由 Vendor Key 的 `accessible_endpoints` + ModelGroup 的 `target_protocol` 决定，路由阶段选择匹配的 Key（详见 ADR-021）。

- **支持矩阵（v1 强制，FRD §2.4）**：
  | 协议 | 客户端入站 | 上游出站 | 关键端点 |
  |---|---|---|---|
  | OpenAI Chat Completions | ✓ | ✓ | `/v1/chat/completions`、`/v1/embeddings`、`/v1/models` |
  | Anthropic Messages | ✓ | ✓ | `/v1/messages`、`/v1/messages?stream=true` |
  | Gemini Generate Content | ✓ | ✓ | `/v1beta/models/{model}:generateContent`、`:streamGenerateContent` |
  | OpenAI Responses API / Cohere / 文心 / 通义 | — | — | v2 演进 |

- **实现位置（对齐 ADR-018）**：
  - `gw-core`：IR 类型 + `ProtocolCodec` trait + `ErrorIR`（详见 ADR-020）+ `AnyCodec` enum 分派 + `Mutations` 类型
  - `gw-codec-macros`（proc-macro = true）：`IrConvert` / `IrStreamConvert` / `IrErrorConvert` / `DetectRequest` 四个 derive 宏 + `#[ir(...)]` / `#[detect(...)]` 属性解析
  - `gw-codec`：`mod openai` / `mod anthropic` / `mod gemini`，每个 mod：(a) 定义 wire-format struct + `#[derive(IrConvert)]`；(b) 手写 via 函数（`openai_msg_vec` 等）做复合字段转换；(c) `pub struct OpenAi` impl `ProtocolCodec`，trait 方法体调用 sonic-rs + derive 生成的 from/into
  - `gw-proxy`：`request_filter` 调 `detect_request` + `parse_request`；`upstream_request_filter` 调 `same_protocol_passthrough` 或 `cross_protocol_forward`；`response_body_filter` 增量调 `parse_stream_chunk` + `render_stream_chunk`

- **业务规则**：
  - 外部 wire-format struct 使用 `Cow<'a, str>` / `&'a RawValue`，避免堆分配；
  - IR 不携带任何路由/计费上下文，路由上下文挂在 `ReqCtx`（在 `gw-proxy::ctx` 中）；
  - IR 版本演进通过新增 `Option` 字段实现，**不允许**破坏已有字段语义（FRD §2.2 业务规则）；
  - IR 新增字段时，**所有 wire-format struct 必须显式声明该字段如何映射**（包括"丢弃"作为合法选项 = 不在该 struct 上标记 `#[ir(map = ...)]`）；
  - **禁止手写字段映射**：所有 `From<X> for IR` / `TryFrom<&IR> for X` 必须经 derive 宏生成；仅 via 函数（叶子级转换）允许手写。code review checklist 强制此约束。

- **验收标准（对齐 FRD）**：
  - AC-2.3-1：OpenAI→OpenAI、8KB payload、无字段 mutation：P50 ≤ 200µs、P99 ≤ 800µs（不含上游 RTT）。同协议 + 空 Mutations 路径走 `Cow::Borrowed(body)`，零 IR 构造。
  - AC-2.3-2：OpenAI→Anthropic、8KB payload：P50 ≤ 800µs、P99 ≤ 2ms。
  - AC-2.3-3：新增第 4 个协议 = 在 `gw-codec` 新增一个 mod（含 wire-struct + derive + via 函数 + ProtocolCodec impl），不动其他 crate；`gw-codec-macros` 不需要修改（除非新协议引入了 IR 没有的字段类型）。
  - AC-2.3-4：IR 新增字段时，未为新字段添加映射的 wire-struct 在编译期警告（derive 宏生成 `#[deprecated(note = "IR field foo not mapped")]` 提示），可由配置切换为错误。
  - AC-019-5：`cargo expand --package gw-codec` 输出中，所有 derive 生成的 `From` / `TryFrom` 函数体均标 `#[inline(always)]`；`cargo asm` 验证 LTO 后无 `serde_json::Value::get` / `Map::get_key_value_str` 调用。

- **备选 A（已采用）**：derive 宏 + 各协议 wire-struct + `AnyCodec` enum 分派。
- **备选 B**：手写 `parse_request` 用 `sonic-rs::get_from_str` 抽字段。**否决**：用户反馈"IR 转换必须走过程宏自动生成内联的转换代码，而不是在运行时产生性能开销"；sonic-rs 字段抽取虽 SIMD 但仍是运行时字符串匹配；derive 宏让字段映射在编译期完全固化。
- **备选 C**：用 `serde_json::Value` 作为 IR。**否决**：字段访问全是 `get(...)` + downcast，无类型安全；mutation 时频繁 `Map::insert` 引起 hash 重排；同协议透传完全无法做（永远 round-trip serialize）。
- **备选 D**：单独为每个客户端 / 上游对手写一份转换函数（N×M 个）。**否决**：FRD §2 明确否决，是组合爆炸根源。
- **备选 E**：基于 `protobuf` 定义 IR 并生成代码。**否决**：JSON 上游不可能直接序列化为 protobuf；引入 protoc 工具链 + 跨语言成本；不解决"未识别字段保留"的需求。
- **备选 F**：codec 实现拆三个 crate（`gw-codec-openai` 等）。**否决**：见 ADR-018 备选 C 与 D，拆分强制 dyn dispatch 或跨 crate 单态化，违反"crate 划分服务于简化设计"原则。
- **备选 G**：把 derive 宏写在 `gw-codec` 内（不独立 `gw-codec-macros`）。**否决**：Rust 语言强约束——proc-macro crate 必须 `proc-macro = true`，无法与普通 lib code 共存（参见 ADR-018 该例外说明）。
- **未来重评信号**：
  - 协议数量超过 8 个且各协议字段差异极大 → 评估按"协议家族"分多 crate（如 openai-compat / anthropic-family）；
  - 同协议透传场景占比下降（< 30%）→ 评估去掉 `same_protocol_passthrough` 优化路径，统一走 derive 宏；
  - IR 在 v2 引入跨字段强约束（如"`tools` 非空时 `tool_choice` 必填"）→ 评估改用类型状态机（typestate pattern）而非纯字段聚合；
  - derive 宏展开后单态化代码膨胀严重（cargo expand 行数 > 5000 / 协议）→ 评估让 macro 生成更紧凑的 helper 函数（用 generic helpers 替代 inline-everything）。

### ADR-020 错误层 ErrorIR + 稳定错误码字典（新增）

> **驱动需求**：FRD §2.6（错误归一化）。避免 N 个客户端 × M 个上游 = N×M 错误码映射；保证客户端永远看到目标协议规范的错误结构，不暴露上游内部 envelope。

- **决定**：
  - 在 `gw-core` 定义 `ErrorIR` 与 `ErrorCategory`：
    ```rust
    pub struct ErrorIR<'a> {
        pub category: ErrorCategory,
        pub status: u16,                        // 网关对客户端的最终 HTTP 码
        pub upstream_status: Option<u16>,
        pub code: Cow<'a, str>,                 // 稳定错误码 GW_*
        pub message: Cow<'a, str>,
        pub retryable: bool,                    // 重试器判定输入（ADR-022）
        pub upstream_raw: Option<&'a RawValue>, // debug 模式透传
        pub request_id: &'a str,
        pub retry_after_secs: Option<u32>,      // 429 / 503 含 Retry-After
    }

    pub enum ErrorCategory {
        ClientBadRequest,   // 4xx 用户原因
        ClientAuth,         // 401/403
        ClientQuota,        // 429 / 402
        UpstreamError,      // 5xx 上游异常
        UpstreamTimeout,    // 网关 / 上游超时
        UpstreamOverloaded, // 上游过载
        GatewayInternal,    // 网关自身 bug
        Cancelled,          // 客户端断开 / 主动取消
    }
    ```
  - **错误流**（两阶段）：
    1. **上游错误 → ErrorIR**：`UpstreamCodec::parse_error(status, body) -> ErrorIR`（ADR-019）。每个 codec impl 知道如何识别自家上游的错误体（如 OpenAI `{"error":{...}}` / Anthropic `{"type":"error","error":{...}}` / Gemini `{"error":{...}}`）。
    2. **ErrorIR → 客户端响应**：`ClientCodec::render_error(&ErrorIR, &mut BytesMut)`。客户端永远收到目标协议规范的错误体（如 OpenAI 客户端永远是 `{"error":{"type":..., "message":..., "code":...}}`，即便上游是 Anthropic）。

- **稳定错误码字典（GW_* prefix，对齐 FRD §2.6.3）**：

  | 错误码 | HTTP | category | retryable | 触发场景 |
  |---|---|---|---|---|
  | `GW_AUTH_INVALID_JWT` | 401 | ClientAuth | ❌ | JWT 验签失败 / 过期 / kid 不存在 |
  | `GW_AUTH_INVALID_APIKEY` | 401 | ClientAuth | ❌ | API Key 不存在 / 状态非 ACTIVE / 哈希不匹配 |
  | `GW_AUTH_NO_KEY_PERMISSION` | 403 | ClientAuth | ❌ | API Key 无权访问该模型/端点 |
  | `GW_AUTH_IP_BLOCKED` | 403 | ClientAuth | ❌ | 命中 IP 黑名单（FRD §11.1） |
  | `GW_AUTH_IP_NOT_ALLOWED` | 403 | ClientAuth | ❌ | 未命中 IP 白名单 |
  | `GW_AUTH_SIGNATURE_INVALID` | 401 | ClientAuth | ❌ | 请求签名校验失败（FRD §11.2） |
  | `GW_QUOTA_BALANCE_ZERO` | 402 | ClientQuota | ❌ | 余额估算冻结失败（ADR-004） |
  | `GW_QUOTA_RPM_EXCEEDED` | 429 | ClientQuota | ✓（带 Retry-After） | 用户/Key/分组/IP 级 RPM 限流 |
  | `GW_QUOTA_TPM_EXCEEDED` | 429 | ClientQuota | ✓ | TPM 限流 |
  | `GW_QUOTA_CONCURRENT_EXCEEDED` | 429 | ClientQuota | ✓ | 用户并发流式连接超限（FRD §5.5） |
  | `GW_REQUEST_PROTOCOL_UNKNOWN` | 400 | ClientBadRequest | ❌ | `detect_request` 识别失败 |
  | `GW_REQUEST_INPUT_TOO_LARGE` | 413 | ClientBadRequest | ❌ | 输入 token 估算超 `max_input_tokens` |
  | `GW_REQUEST_COST_TOO_HIGH` | 413 | ClientBadRequest | ❌ | 单请求预估成本超 `single_request_max_cost_usd` |
  | `GW_REQUEST_CONTENT_REJECTED` | 400 | ClientBadRequest | ❌ | 内容审计命中拦截（FRD §11.4） |
  | `GW_REQUEST_MODEL_NOT_FOUND` | 404 | ClientBadRequest | ❌ | model 在 pricing 表中不存在 |
  | `GW_ROUTING_NO_AVAILABLE_CHANNEL` | 503 | UpstreamError | ❌（无其它 Key 可换） | 模型分组内无可用 Vendor Key |
  | `GW_UPSTREAM_TIMEOUT` | 504 | UpstreamTimeout | ✓（非流式）/ ❌（流式中途） | 网关 connect / read / total / stream_total 任一超时 |
  | `GW_UPSTREAM_5XX` | 502 | UpstreamError | ✓ | 上游 5xx（重试用尽后返回客户端） |
  | `GW_UPSTREAM_4XX` | 透传 | ClientBadRequest | ❌ | 上游 4xx（不重试，透传给客户端，不计费） |
  | `GW_UPSTREAM_OVERLOADED` | 503 | UpstreamOverloaded | ✓ | Anthropic `overloaded_error` / OpenAI server overloaded |
  | `GW_UPSTREAM_CANCELLED` | 499 | Cancelled | ❌ | 客户端在响应完成前断开（不计入失败率） |
  | `GW_BACKEND_UNAVAILABLE` | 503 | GatewayInternal | ❌ | Redis + PgSQL 同时不可达（冷加载失败） |
  | `GW_GATEWAY_INTERNAL` | 500 | GatewayInternal | ❌ | 网关自身 bug（panic / 解码失败 / 应监控） |

- **业务规则**：
  - 网关错误码为**稳定 API 契约**，不允许在小版本迭代中变更含义；新增错误码不破坏既有码（FRD §2.6.3）。
  - `retryable` 由 `ErrorIR` 构造时判定，依据（FRD §2.6.2）：
    - HTTP 502/503/504、network error、connect refused → retryable
    - HTTP 429 → retryable（带 `Retry-After`）
    - HTTP 4xx（除 429）→ not retryable
    - 流式中途上游断 → not retryable（无法续接已部分发送的流，ADR-019）
  - 客户端断开（`Cancelled`）不计入失败率、不进入熔断器统计（ADR-008）、不计费（ADR-004 失败矩阵）。
  - **错误透传**：debug 模式（`config.logging.debug_error_passthrough=true`）下 `upstream_raw` 透传到客户端响应体的 `error.upstream` 字段；生产模式默认隐藏。
  - **错误响应头**：
    - 所有错误响应必含 `X-Request-Id: <trace_id>` 头
    - 限流类错误必含 `Retry-After: <seconds>` + `X-RateLimit-Limit` / `X-RateLimit-Remaining` / `X-RateLimit-Reset`（FRD §5.4）
    - 客户端断开类错误（`Cancelled`）不发送响应（连接已断）

- **实现位置（对齐 ADR-018）**：
  - `gw-core`：`ErrorIR` / `ErrorCategory` / 稳定错误码常量（`pub const GW_AUTH_INVALID_JWT: &str = "GW_AUTH_INVALID_JWT";` 风格）
  - `gw-codec`：每个 codec impl 内的 `parse_error` / `render_error`
  - `gw-proxy`：在 filter 阶段构造 `ErrorIR`（非上游错误的场景，如 quota / auth），调用 client codec 的 `render_error` 写响应体；`fail_to_proxy` 钩子统一处理网关侧异常 → `GW_GATEWAY_INTERNAL`
  - `gw-routing`：`RetryExecutor` 读 `ErrorIR.retryable` 决定是否重试（ADR-022）
  - `gw-ledger`：失败请求按 `ErrorCategory` 决定是否解冻 / 计费（ADR-004 失败矩阵）

- **验收标准（对齐 FRD §2.6）**：
  - AC-2.6-1：Anthropic 返回 HTTP 529 `overloaded_error` → 网关记录为 `GW_UPSTREAM_OVERLOADED`，`retryable=true`，触发跨 Key 重试。
  - AC-2.6-2：客户端 OpenAI 协议、上游 Anthropic 返回 `invalid_request_error` → 客户端收到 OpenAI 风格 400 错误体（`error.type=invalid_request_error`），不暴露 Anthropic envelope。
  - AC-2.6-3：所有 `retryable` 判定由 `ErrorCategory` 的 `match` 穷尽性保证（编译期）。

- **备选 A（已采用）**：ErrorIR + ProtocolCodec parse_error/render_error + 稳定 `GW_*` 字典。
- **备选 B**：直接透传上游错误体到客户端。**否决**：违反 FRD §2.6 "客户端永远收到目标协议规范的错误体"；让客户端 SDK 难以稳定处理（OpenAI SDK 收到 Anthropic 错误结构会崩）。
- **备选 C**：错误码用 HTTP status 当作唯一标识，不引入 `GW_*` 字符串码。**否决**：HTTP 4xx/5xx 在不同场景含义不同（429 既可能是 RPM 也可能是 TPM 也可能是并发），需要更细粒度的稳定标识便于告警与客户端处理。
- **未来重评信号**：
  - 错误码字典超过 50 条 → 评估按 category 分模块整理；
  - 上游协议出现网关无法识别的新错误结构 → 当作 `GW_UPSTREAM_5XX` + `XADD alerts` 记录原始 envelope 由人工分析。

### ADR-021 Vendor 实体 + KeyEntry 扩列 + 7 状态机（新增 / 取代 v2026-05-14 ADR-017 部分内容）

> **驱动需求**：FRD §7.1（模型分组与 Vendor Key 多维聚合）+ §8.1（供应商基础连接）+ §9（上游密钥管理）。原 v2026-05-14 `KeyEntry` 只有 9 个字段、状态 3 值；FRD 要求扩展为 13+ 字段、7 状态机，并把"供应商基础信息"独立为 `Vendor` 实体。

- **决定**：
  - **三层数据结构**（Entity 协议，namespace 分别为 `vendor` / `vendor_key` / `model_group`）：

    ```rust
    /// 供应商：仅持有"基础连接信息"，不持有模型/端点格式
    pub struct Vendor {
        pub id: VendorId,
        pub display_name: String,
        pub base_urls: Vec<Url>,                 // 多 base_url 主备（FRD §8.1）
        pub auth_template: AuthTemplate,         // 例: "Authorization: Bearer {api_key}"
        pub timeout_overrides: Option<TimeoutSpec>,
        pub tls_config: TlsConfig,               // CA / 客户端证书 / mTLS
        pub default_proxy: Option<Url>,
        pub contact: Option<String>,             // 供应商联系方式（非热路径）
    }

    /// Vendor Key：自包含的请求处理单元
    pub struct VendorKey {
        pub key_id: KeyId,
        pub vendor_id: VendorId,                 // 继承 base_url 与默认代理
        pub api_key: SecretString,               // AES-256-GCM 解密后的明文，受 ADR-017 保护
        pub accessible_models: Vec<ModelName>,   // 该 Key 可服务的统一模型标识
        pub model_mapping: HashMap<ModelName, String>, // 统一名 → 供应商侧名（FRD §7.2）
        pub accessible_endpoints: Vec<ProtocolKind>,   // [OpenAi, Anthropic, Gemini]
        pub protocol_conversion: bool,           // 是否允许跨协议转换
        pub priority: i32,                       // [-100, 100]，越大越优先
        pub weight: u32,                         // SWRR 权重 [1, 1000]，0=临时禁用
        pub rpm_limit: u32,                      // 0 表示不限
        pub tpm_limit: u32,                      // 0 表示不限
        pub max_usage_count: Option<u64>,        // 累计使用次数上限（FRD §9.6）
        pub proxy: Option<Url>,                  // 覆盖 vendor 默认代理
        pub ip_allowlist: Option<Vec<IpCidr>>,   // 最多 20 条（FRD §9.4）
        pub expires_at: Option<DateTime<Utc>>,   // 到期时间，触发 expired
        pub status: KeyStatus,                   // 见 7 状态机
        pub tags: Vec<String>,                   // 路由标签（FRD §8.2）
    }

    pub enum ProtocolKind { OpenAi, Anthropic, Gemini }

    /// 模型分组：按业务维度组织 Vendor Key
    pub struct ModelGroup {
        pub group: Group,
        pub strategy: StrategyKind,              // 8 策略之一（ADR-003）
        pub cross_vendor_fallback: bool,         // 跨 Vendor Fallback 开关
        pub max_tokens_cap: Option<u32>,         // group 级 max_tokens 上限（FRD §7.6）
        pub default_params: DefaultParams,       // temperature / top_p 默认值
        pub forced_params: ForcedParams,         // 强制覆盖参数
        pub vendor_key_ids: Vec<KeyId>,          // 引用，懒解析为 Vec<Arc<VendorKey>>
    }
    ```

- **7 状态机（FRD §9.3）**：

  ```
                ┌────────────┐
                │ active ◄───┼─────── rate_limited（窗口结束）
                │   │ │ │ │  │       expired（管理员延期）
                │   │ │ │ │  │       depleted（重置后）
                └───┼─┼─┼─┼──┘
            管理员禁│ │ │ │
                   ▼ │ │ │
              disabled│ │ │
                   │  │ │ │
                到期│  │ │ │
                   │  ▼ │ │
              expired   │ │
                   │    │ │
              用量│    │ │
              耗尽│    ▼ │
              depleted   │
                   │    │ │
                   │ 触限│ │
                   │   ▼ │
                   │ rate_limited ──── 60s 后自动 → active
                   │     │
                   │  轮换│
                   │     ▼
                   │  rotated（保留 7 天）
                   │     │
                   ▼     ▼
                deleted（终态）
  ```

  | 状态 | 可调度 | 触发条件 | 转换目标 | 写入方 |
  |---|---|---|---|---|
  | `active` | ✓ | 默认初始态 | disabled / expired / depleted / rate_limited | 管理面（Go） |
  | `disabled` | ❌ | 管理员手动停用 | active / deleted | 管理面 |
  | `expired` | ❌ | 当前时刻 > `expires_at` | active（延期后） / deleted | 网关每秒扫描自动检测 + 管理面 |
  | `depleted` | ❌ | `counter:vendor_key:<id>:used_count ≥ max_usage_count` | active（重置后） / deleted | 网关计数器达上限触发 + 管理面 |
  | `rate_limited` | ❌ | RPM/TPM 触限 | active（窗口结束自动）| 网关本地判定 |
  | `rotated` | ❌ | 轮换后旧 Key 保留 7 天 | deleted | 管理面 |
  | `deleted` | ❌ | 删除（终态） | — | 管理面 |

- **业务规则**：
  - 网关每秒扫描一次 Key 状态机，自动处理 `rate_limited → active` 与 `expired` 检测（FRD §9.3）。
  - 所有状态变更的"原因"通过 `events:entity`（op=upsert）同步给管理面记审计。
  - `rate_limited` 状态由网关本地判定 + 异步 `XADD events:entity ... op=status_change reason=rate_limited`；其他状态由管理面权威写入。
  - **权限校验顺序**（FRD §9.4，最轻量先行）：端点 → 模型 → IP → 速率 → 使用次数。任一不通过 → 不进入候选池。
  - Key 被禁用 / 删除时，模型分组的候选列表自动剔除该 Key，分组本身不失效（只要还有至少一个可用 Key，FRD §7.1.2）。

- **Vendor 多 base_url 主备**（FRD §8.1）：
  - 网关健康检查 worker 每 30s 探测每个 `base_urls[i]` 可达性；主地址不可用时自动切换备地址，切换时间 ≤ 15s。
  - 当前生效 base_url 缓存于 `DashMap<VendorId, ActiveBaseUrl>`，每实例独立判定（不跨实例同步）。

- **Vendor Key 解密 + 加载链**（与 ADR-017 协同）：
  ```
  entity:vendor_key:<id> Redis HASH 内容：
      v: u64
      enc: "json"
      d: <JSON 序列化的 EncryptedKeyEntry>

  EncryptedKeyEntry {
      key_id, vendor_id,
      ciphertext: <base64>,
      nonce: <base64>,
      auth_tag: <base64>,
      kek_kid: <string>,
      // ... 其他非敏感字段
  }

  cold-path 解析：sqlx SELECT → EncryptedKeyEntry → 解密 api_key → KeyEntry { api_key: SecretString, ... }
  Moka 缓存形态：Arc<Versioned<KeyEntry>>（含明文 SecretString）
  Redis 写回形态：ciphertext + nonce + auth_tag + kek_kid（永远密文）
  ```

- **`get_candidate_keys` 实现（在 `gw-store::ModelGroup`）**：
  ```rust
  impl ModelGroup {
      pub fn get_candidate_keys(
          &self,
          model: &ModelName,
          target_protocol: ProtocolKind,
          perm: &MergedPermissions,           // 来自 gw-auth（ADR-023）
          breakers: &BreakerStore,             // 来自 gw-routing
          vendor_key_cache: &EntityCache<VendorKey>,
      ) -> Vec<Arc<VendorKey>> {
          self.vendor_key_ids.iter()
              .filter_map(|kid| vendor_key_cache.get_cached(kid))
              .filter(|key| key.status == KeyStatus::Active)
              .filter(|key| key.accessible_models.contains(model))
              .filter(|key| {
                  key.accessible_endpoints.contains(&target_protocol)
                      || key.protocol_conversion
              })
              .filter(|key| perm.allowed_channels.contains(&key.key_id))
              .filter(|key| ip_allowed(key, &perm.client_ip))
              .filter(|key| {
                  breakers.in_flight(&key.key_id) < BREAKER_IN_FLIGHT_PER_KEY
              })
              .collect()
      }
  }
  ```

- **实现位置（对齐 ADR-018）**：
  - `gw-core`：`KeyStatus` enum / `ProtocolKind` enum / `VendorId` / `KeyId` newtype
  - `gw-store`：`Vendor` / `VendorKey` / `ModelGroup` 类型定义、`EntityCache<VendorKey>` 懒加载、`get_candidate_keys` 实现、`EncryptedKeyEntry` 解密
  - `gw-routing`：使用 `Vec<Arc<VendorKey>>` 候选列表做 Strategy.pick
  - `gw-proxy`：upstream_peer 中编排"perm → candidates → pick → 构造 HttpPeer（按 Vendor.base_urls 选 active + Vendor.tls_config）"

- **备选 A（已采用）**：三层 entity（Vendor / VendorKey / ModelGroup），KeyEntry 13+ 字段，7 状态机。
- **备选 B（v2026-05-14）**：把 base_url / provider 都塞进 KeyEntry。**否决**：FRD §7.1 / §8.1 明确"供应商仅管理基础连接信息""模型权限聚合在 Key"——拆分后修改 Vendor 端点不需要逐个改 Key，符合 FRD 多维聚合原则。
- **备选 C**：状态机只用 3 值（active / disabled / maintenance），其他靠计数器表达。**否决**：FRD §9.3 明确 7 状态，且各状态有不同的转换条件与权威写入方；折叠为 3 值会丢失"用量耗尽 vs 限流 vs 到期"的语义差异，告警与运维难。
- **未来重评信号**：
  - Vendor 的 base_url 主备策略需要按地域 / 延迟动态选择 → 引入 ActiveBaseUrl 共享状态（Redis）或健康检查打分；
  - Key 状态机出现新场景（如"灰度发布中"）→ 评估在 `KeyStatus` 增枚举变体，所有 match 处编译期穷尽性保证。

### ADR-022 重试策略 = 同 Key / 跨 Key / 混合 + 幂等保护（新增）

> **驱动需求**：FRD §7.5（重试策略）+ §10.5（重试不重复扣费）+ §3.3（流式中途失败不重试）。

- **决定**：
  - 重试由独立的 `RetryExecutor` 驱动（`gw-routing` crate），它消费 `ErrorIR.retryable`（ADR-020）和 Strategy.pick 结果做循环：
    ```rust
    pub struct RetryConfig {
        pub mode: RetryMode,            // SameKey | CrossKey | Mixed（默认）
        pub same_key_max: u32,           // 默认 2
        pub cross_key_max: u32,          // 默认 3
        pub backoff_base_ms: u32,        // 默认 500
        pub backoff_max_ms: u32,         // 默认 10_000
        pub cross_vendor_fallback: bool, // 来自 ModelGroup 配置
        pub total_budget_ms_nonstream: u32, // 默认 60_000
        pub total_budget_ms_stream:    u32, // 默认 600_000
    }
    pub enum RetryMode { SameKey, CrossKey, Mixed }
    ```
  - **三种重试模式（FRD §7.5）**：
    - `SameKey`：单 Key 上重试（适合 Key 自身瞬时故障）。
    - `CrossKey`：每次失败切到分组内下一个候选 Key（适合 Key 永久故障）。
    - `Mixed`（默认）：先 SameKey 重试 `same_key_max` 次；仍失败则 CrossKey 切下一个 Key，新 Key 上再 SameKey 重试，直到 `cross_key_max` 个 Key 都试过或 `total_budget` 耗尽。

- **执行流程**：
  ```
  let mut attempts = 0;
  let mut visited_keys = HashSet::new();
  let deadline = Instant::now() + total_budget;

  loop {
      // 1. 选 Key
      let candidates = ModelGroup::get_candidate_keys(...)
          .into_iter()
          .filter(|k| !visited_keys.contains(&k.key_id))
          .collect();
      let key = Strategy::pick(&candidates, &breakers, &ctx)
          .ok_or(GW_ROUTING_NO_AVAILABLE_CHANNEL)?;
      visited_keys.insert(key.key_id.clone());

      // 2. 发请求
      let result = upstream_call(key, &ir, deadline).await;

      // 3. 判定
      match result {
          Ok(resp) => return Ok(resp),
          Err(e) if !e.retryable => return Err(e),     // 4xx 立即返回
          Err(e) if Instant::now() >= deadline => return Err(GW_UPSTREAM_TIMEOUT),
          Err(e) if is_stream_partial(&ctx) => return Err(e),  // 流式中途断（FRD §3.3）
          Err(e) => {
              attempts += 1;
              if attempts >= cross_key_max && visited_keys.len() >= cross_key_max {
                  return Err(map_to_final_error(e));
              }
              tokio::time::sleep(backoff(attempts)).await;
          }
      }
  }
  ```

- **退避算法**：
  ```
  backoff(attempt) = min(backoff_base_ms × 2^(attempt-1), backoff_max_ms)
                  + jitter(0..backoff_base_ms/2)
  ```
  抖动避免 thundering herd。

- **流式语义（FRD §3.3 / §3.4）**：
  - 流式请求**已发出首个 chunk** → 不再重试（无法续接已部分发送的流）；
  - 流式请求**connect 阶段失败** → 可重试（客户端还没收到任何字节，重试透明）；
  - `is_stream_partial(&ctx)` 判定：`ctx.is_stream && ctx.bytes_sent_to_client > 0`。

- **幂等保护（FRD §7.5 业务规则）**：
  - 非幂等请求 = 请求体含 `tool_calls`（assistant 在主动调用工具，重试会重复执行副作用）；
  - 非幂等请求的重试规则：仅当**网络层 connect 失败 / TCP RST**（请求未到达上游）时重试，**收到任何明确错误响应**（含 5xx）时不重试；
  - 实现：`RequestIR::has_side_effects()` 检查 `messages` 中是否有 `MessageIR::Assistant { tool_calls: Some(_) }`，结果存入 `ReqCtx.idempotent: bool`；
  - `RetryExecutor` 在判定 retry 前先看 `ctx.idempotent`，非幂等时只对 `ErrorCategory::UpstreamTimeout`（含 connect timeout）和网络层错误重试。

- **重试不重复扣费（FRD §10.5）**：
  - **冻结只发生一次**：`request_filter` 阶段进入估算冻结后，整个请求生命周期（含所有重试）frozen_micro 不再变化；
  - 重试目标 Key 的成本可能不同：actual_micro 按**最终成功的 Key 的定价**结算，多退少补由 `FreezeGuard::settle` 完成（ADR-004）；
  - 失败的中间重试不进入 `billing:records`，仅有最终结果（成功或全部失败）写 1 条 BillingRecord，含 `retry_count` 字段记录次数。

- **熔断器协同（与 ADR-008）**：
  - 每次失败 → `BreakerState.record_failure(key_id)`；连续失败可能触发该 Key Open；
  - `Strategy::pick` 自动跳过 Open 状态的 Key（candidates 已过滤）；
  - HalfOpen 状态的 Key 在重试中也会被选中（probe），探活成功 → Close，失败 → 立即 Open + 60s 计时重置。

- **跨 Vendor Fallback**（FRD §7.3.8 + §13.1）：
  - 默认关闭（`cross_vendor_fallback: false`）以保持厂商一致性（用户期待同一供应商的输出风格）；
  - 启用时：候选列表扩展到不同 Vendor 的 Key（但仍受 `accessible_models` 过滤）；
  - 跨 Vendor 时若涉及协议转换（如 OpenAI 主上游降级到 Anthropic 备份），需要 Key 的 `protocol_conversion: true`。

- **实现位置（对齐 ADR-018）**：
  - `gw-core`：无（RetryConfig 类型可放此处但属于配置层）
  - `gw-routing`：`RetryExecutor` / `RetryConfig` / `RetryMode`、退避算法、流式分支判定
  - `gw-proxy`：在 `proxy_upstream_filter` / 上游 connect 错误回调中调 `RetryExecutor::next_attempt()`；Pingora 0.8 的 ProxyHttp 重试通过 `fail_to_proxy` + 自管 loop 实现

- **业务规则**：
  - 总超时（含所有重试）默认 60s（非流式）/ 600s（流式 total），上限可配（FRD §7.5）。
  - 任一重试达 `total_budget_ms` → 立即返回 `GW_UPSTREAM_TIMEOUT`，不再尝试。
  - 重试日志：每次重试 attempts 增加 1，trace span 记录每次重试的 key_id / 失败原因；`billing:records` 含 `retry_count` 字段。

- **验收标准**：
  - AC-022-1：4xx 错误（非 429）立即返回客户端，不重试。
  - AC-022-2：5xx 错误触发 Mixed 重试，全失败后返回 `GW_UPSTREAM_5XX`，billing 含 `retry_count = 5`（2 SameKey + 3 CrossKey）。
  - AC-022-3：流式首 chunk 发送后上游断 → 立即返回 error event 并关闭流，不重试。
  - AC-022-4：含 `tool_calls` 的请求收到 5xx → 不重试，直接 502 透传。
  - AC-022-5：重试成功后 `billing:records.cost_micro` 按成功 Key 的定价计算，冻结金额差额已解冻。

- **备选 A（已采用）**：Mixed 模式默认 + 幂等保护 + 流式不重试。
- **备选 B**：纯 CrossKey（每次失败必换 Key）。**否决**：某些瞬时故障（短暂 5xx）在同 Key 上立即重试比换 Key 更高效；FRD §7.5 列出三种模式给运营选择。
- **备选 C**：Pingora 内置 retry。**否决**：Pingora 的 retry 是 transport 层（连接级），不感知应用语义（不知道 `tool_calls` 的副作用、不知道流式 vs 非流式）；必须在应用层包一层 RetryExecutor。
- **备选 D**：用 `tower::retry` 库。**否决**：tower 的 retry middleware 与 pingora 的 ProxyHttp 模型不兼容；自己实现成本 < 100 行，避免依赖。
- **未来重评信号**：
  - 出现"重试导致雷击效应"（同一上游瞬时故障引发所有客户端集中重试）→ 引入全局并发重试上限或 token bucket；
  - 跨 Vendor Fallback 出现"输出风格不一致引发用户投诉" → 引入"软关闭"策略（先通知客户端切了 Vendor，由客户端决定是否接受）。

### ADR-023 用户分组 + 子账户 + 五级权限合并（新增）

> **驱动需求**：FRD §6.1（权限层级，从低到高五级）+ §6.2（合并算法）+ §6.3（子账户）。

- **决定**：
  - **五级权限层级**（从低到高）：
    | 层级 | 来源 | 网关读取方式 | 缓存 |
    |---|---|---|---|
    | 系统全局 | `config.toml` 默认值 | 启动加载 | 进程级（不变） |
    | 用户分组 | `entity:user_group:<id>` | 懒加载（一个用户可属多个分组） | Moka |
    | 主账户 | `entity:user:<id>` | 懒加载 | Moka |
    | 子账户 | `entity:user:<id>`（带 `parent_id` 字段） | 懒加载 | Moka |
    | API Key | `entity:apikey:<id>` | 懒加载（JWT 或 APIKey 之一） | Moka |

  - **核心类型**（`gw-core`）：
    ```rust
    pub struct UserEntry {
        pub user_id: UserId,
        pub parent_id: Option<UserId>,           // None = 主账户，Some = 子账户
        pub tier: Tier,
        pub status: UserStatus,                  // Active | Banned
        pub allowed_models: Vec<ModelName>,      // 白名单（空 = 全部）
        pub allowed_channels: Vec<KeyId>,        // 渠道白名单（空 = 全部）
        pub allowed_groups: Vec<Group>,          // 模型分组白名单
        pub rpm_limit: u32,                       // 0 = 不限
        pub tpm_limit: u32,
        pub max_concurrent_streams: u32,
        pub daily_quota_tokens: u64,
        pub user_group_ids: Vec<UserGroupId>,    // 所属用户分组（FRD §6.3.2 多组）
        pub markup: f32,                          // 用户个人倍率（FRD §10.2）
    }

    pub struct UserGroup {
        pub id: UserGroupId,
        pub display_name: String,
        pub priority: i32,                        // 模型分组绑定的优先级（FRD §6.3.2）
        pub allowed_models: Vec<ModelName>,
        pub allowed_channels: Vec<KeyId>,
        pub model_group_binding: Option<Group>,   // 强制路由到的模型分组
        pub rpm_limit: u32,
        pub tpm_limit: u32,
        pub max_concurrent_streams: u32,
        pub markup: f32,                          // 用户分组倍率
    }

    pub struct ApiKeyEntry {
        pub key_id: ApiKeyId,
        pub user_id: UserId,                      // 关联到用户
        pub status: ApiKeyStatus,                 // Active | Disabled
        pub hash: Vec<u8>,                        // bcrypt / argon2 哈希后比对
        pub allowed_models: Vec<ModelName>,
        pub allowed_channels: Vec<KeyId>,
        pub rpm_limit: u32,
        pub tpm_limit: u32,
        pub markup: f32,                          // API Key 级倍率
        pub ip_allowlist: Option<Vec<IpCidr>>,
        pub expires_at: Option<DateTime<Utc>>,
    }
    ```

  - **合并算法**（`gw-auth::merge_permissions`，FRD §6.2）：

    | 权限类型 | 合并规则 |
    |---|---|
    | 可用模型 | `key.models ∩ user.models ∩ ⋂(groups[i].models)`（**交集**） |
    | 可用渠道 | 同上 |
    | 可用模型分组 | 同上 |
    | RPM | `min(key.rpm, user.rpm, min(groups[i].rpm), global.rpm)`（**取最严**） |
    | TPM | 同 RPM |
    | 并发流式上限 | 同 RPM |
    | 倍率（计费）| `key.markup × user.markup × group_max.markup × distributor.markup`（**连乘**） |
    | 模型分组绑定 | 用户分组 `priority` 最高的 `model_group_binding` wins（FRD §6.3.2） |

    ```rust
    pub struct MergedPermissions {
        pub user_id: UserId,                      // 子账户的话是 child_id
        pub effective_user_id: UserId,            // 计费实际用的 UserId（子账户 = parent_id）
        pub client_ip: IpAddr,
        pub allowed_models: HashSet<ModelName>,
        pub allowed_channels: HashSet<KeyId>,
        pub allowed_groups: HashSet<Group>,
        pub forced_model_group: Option<Group>,
        pub rpm_limit: u32,
        pub tpm_limit: u32,
        pub max_concurrent_streams: u32,
        pub markup_product: f32,
    }

    pub async fn merge_permissions(
        ctx: &AuthCtx,                            // 已验签后的身份
        store: &EntityStore,
    ) -> Result<MergedPermissions, ErrorIR<'static>> {
        // 1. 加载主账户（子账户的话加载 parent）
        let user = store.user.get_with(&ctx.user_id).await?;
        let main_user = match &user.parent_id {
            Some(parent) => store.user.get_with(parent).await?,
            None => user.clone(),
        };

        // 2. 加载所有用户分组
        let groups: Vec<_> = futures::future::try_join_all(
            main_user.user_group_ids.iter().map(|gid| store.user_group.get_with(gid))
        ).await?;

        // 3. 加载 API Key（如果是 APIKey 鉴权）
        let api_key = if let Some(akid) = &ctx.api_key_id {
            Some(store.apikey.get_with(akid).await?)
        } else { None };

        // 4. 合并
        let allowed_models = intersect_models(&user, &main_user, &groups, api_key.as_deref());
        let rpm = min_rpm(&user, &main_user, &groups, api_key.as_deref(), GLOBAL_RPM);
        let markup_product = multiply_markups(&user, &main_user, &groups, api_key.as_deref());
        let forced_model_group = groups.iter()
            .max_by_key(|g| g.priority)
            .and_then(|g| g.model_group_binding.clone());

        // 子账户：计费实际用的 UserId 是父账户（FRD §6.3）
        let effective_user_id = user.parent_id.clone().unwrap_or(user.user_id.clone());

        Ok(MergedPermissions { ... })
    }
    ```

- **子账户语义（FRD §6.3）**：
  - **共享余额池**：子账户与主账户共享 `counter:user:<parent_id>:balance_micro`；扣账时实际写父账户的 counter，但 `BillingRecord.child_id` 字段记录是谁产生的消费（便于父账户对账）。
  - **断路联动**：父账户欠费 → 所有子账户同时停服（`USER_CIRCUIT[parent_id]` 一开，子账户请求路径上也走 402）。
  - **限流独立但合并取严**：子账户的 RPM/TPM 在合并算法中独立生效（取 min），子账户**不能**突破父账户限制（即 `min(child.rpm, parent.rpm)`）。
  - **权限隔离但取交集**：子账户的 `allowed_models` 与父账户取交集；管理员给子账户的额外权限不能超过父账户。

- **计算复杂度（FRD §6.2 业务规则）**：
  - 合并耗时预算 P99 ≤ 5µs（≤ 10 个分组场景）。
  - 合并结果**不缓存**到 Moka：避免引入新的失效协议（用户/分组/Key 任一改动都要 invalidate），权衡是 CPU vs 内存复杂度。Moka 已经缓存了源数据，合并是纯内存操作。
  - 每次请求 `request_filter` 阶段调用一次 `merge_permissions`，依赖 Moka 的 Arc 引用计数。

- **配额校验顺序（与 ADR-021 / FRD §9.4 一致）**：
  1. JWT/APIKey 验签（auth）→ 失败 401
  2. IP 黑名单（FRD §11.1，最早拒绝）→ 失败 403
  3. `merge_permissions` 产出 `MergedPermissions`
  4. 模型权限校验：`request.model ∈ perm.allowed_models` → 失败 403 `GW_AUTH_NO_KEY_PERMISSION`
  5. 单请求 hard cap（FRD §5.6）：`estimate_input_tokens ≤ max_input_tokens` + `estimate_cost ≤ single_request_max_cost` → 失败 413（详见 ADR-024，batch 2）
  6. 多级限流（用户/分组/Key/IP RPM/TPM，FRD §5.4）→ 失败 429
  7. 估算冻结（ADR-004）→ 失败 402
  8. 候选 Key 过滤（ADR-021 `get_candidate_keys`）→ 失败 503

- **实现位置（对齐 ADR-018）**：
  - `gw-core`：`UserEntry` / `UserGroup` / `ApiKeyEntry` / `MergedPermissions` 类型
  - `gw-store`：Moka 缓存 + 加载链
  - `gw-auth`：JWT / APIKey 验签 + `merge_permissions` 函数 + IP 校验工具
  - `gw-proxy`：在 `request_filter` 调 `gw-auth::authenticate` → `gw-auth::merge_permissions`，将 `MergedPermissions` 挂到 `ReqCtx`

- **业务规则**：
  - **取交集 vs 取最严**：白名单类（模型/渠道/分组）取交集（任何一层不允许即不允许）；数值上限类（RPM/TPM/并发）取最小（最严的层 wins）；倍率类（markup）连乘。
  - 用户分组的 `priority` 仅用于 `model_group_binding` 的 winner 判定；不影响白名单合并（白名单与 priority 无关）。
  - API Key 是可选的（用户也可用 JWT 直接鉴权），合并时若无 API Key 则跳过 Key 层。
  - 子账户层级仅 1 层（不允许子账户再生子账户）；schema 约束：`parent_id` 必须指向一个 `parent_id IS NULL` 的用户。

- **验收标准（对齐 FRD §6）**：
  - AC-023-1：用户属于 2 个分组（A 允许 GPT-4，B 允许 Claude），主账户允许 GPT-4 + Claude，API Key 允许 GPT-4 → 最终 `allowed_models = {GPT-4}`（交集）。
  - AC-023-2：用户分组 A 的 RPM=100，主账户 RPM=200，API Key RPM=300 → 最终 RPM=100。
  - AC-023-3：子账户消费 → `BillingRecord.user_id = parent_id`，`BillingRecord.child_id = child_user_id`，扣账写 `counter:user:<parent_id>:balance_micro`。
  - AC-023-4：父账户余额耗尽 → 子账户后续请求秒级 402。
  - AC-023-5：合并耗时 P99 ≤ 5µs（10 个分组场景，benchmark）。

- **备选 A（已采用）**：五级合并 + 子账户共享余额 + 不缓存合并结果。
- **备选 B**：缓存 `MergedPermissions` 到 Moka。**否决**：失效协议复杂（任何源 entity 更新都要 invalidate 关联的合并结果）；P99 ≤ 5µs 的合并相对 Moka lookup 没有显著差距。
- **备选 C**：子账户独立余额池。**否决**：FRD §6.3 明确"共享余额池"语义；独立余额池需要额外的子账户额度分配机制（管理面工作量）。
- **备选 D**：取最宽松（OR 合并）而非交集。**否决**：白名单合并的标准语义是"必须所有层都允许"（最小特权）；OR 合并会让 API Key 的"放宽"覆盖用户分组的限制，违反企业用户预期。
- **未来重评信号**：
  - 出现"子账户的子账户"需求 → 评估改用通用树形 `path` 字段（如 `ltree` 列）；
  - 单次合并涉及的用户分组数超过 50 → 评估改用预计算的 effective_permissions 表（管理面写入）。

### ADR-024 多级限流 + 单请求 hard cap + 并发流式上限（新增）

> **驱动需求**：FRD §5.4（RPM/TPM 限流）+ §5.5（单用户并发流式）+ §5.6（单请求 hard cap）+ §11.6（多级限流）。

- **决定**：
  - **限流维度（FRD §5.4 + §11.6）**：
    | 维度 | 默认 | 配置 entity | 校验位置 |
    |---|---|---|---|
    | IP 级 RPM | 600/min | `config.toml` 全局 | `gw-auth` |
    | 用户级 RPM | 600/min | `entity:user.rpm_limit` | `gw-auth` |
    | 用户级 TPM | 200_000/min | `entity:user.tpm_limit` | `gw-auth` |
    | 用户分组级 RPM/TPM | inherit | `entity:user_group.rpm_limit` | `gw-auth` |
    | API Key 级 RPM/TPM | 600/min | `entity:apikey.rpm_limit` | `gw-auth` |
    | Vendor Key 级 RPM/TPM | 0（不限） | `entity:vendor_key.rpm_limit` | `gw-routing`（路由前过滤 rate_limited） |
    | 用户级并发流式 | 100 | `config.runtime.max_concurrent_streams_per_user` | `gw-auth` |
    | 全局并发流式 | 10_000 | `config.runtime.max_total_concurrent_streams` | `gw-proxy::early_request_filter` |

  - **限流算法选型**：
    - 用户/分组/Key/IP 级：**本地令牌桶 + 周期性 Redis 同步**（FRD §5.4 业务规则）。本地 `governor::RateLimiter` 维护 `(scope, id) → Quota` 实例，每 60s 把"本地消耗"flush 到 `counter:ratelimit:<scope>:<id>` 并从 Redis 拉同步快照修正本地余量。
    - 并发流式：本地 `DashMap<UserId, AtomicUsize>`，开流 `fetch_add(1)`，logging/drop 时 `fetch_sub(1)`；不跨实例同步（每实例独立判定）。
  - **限流计数误差**：≤ 1%（FRD §5.4），主要来源是本地 + Redis 同步窗口内的并发偏差，可接受。

  - **单请求 hard cap（FRD §5.6）**：
    | 校验 | 默认 | 错误码 | 时机 |
    |---|---|---|---|
    | `max_input_tokens` | 2_000_000 | `GW_REQUEST_INPUT_TOO_LARGE`（413） | request_filter（IR parse 之后、估算冻结之前） |
    | `single_request_max_cost_usd` | 100.0 | `GW_REQUEST_COST_TOO_HIGH`（413） | request_filter（估算之后、冻结之前） |

  - **数据结构**（`gw-auth::ratelimit`）：
    ```rust
    pub struct RateLimitGuard {
        // 本地 DashMap<(Scope, Id), Arc<governor::RateLimiter>>
        per_user: DashMap<UserId, Arc<governor::RateLimiter>>,
        per_group: DashMap<UserGroupId, Arc<governor::RateLimiter>>,
        per_apikey: DashMap<ApiKeyId, Arc<governor::RateLimiter>>,
        per_ip: DashMap<IpAddr, Arc<governor::RateLimiter>>,

        // 并发流式（精确计数）
        per_user_streams: DashMap<UserId, AtomicUsize>,
        total_streams: AtomicUsize,

        // 周期性同步任务（每 60s）
        flush_handle: JoinHandle<()>,
    }

    impl RateLimitGuard {
        pub fn check_request(&self, perm: &MergedPermissions, ip: IpAddr) -> Result<RateLimitTicket, ErrorIR<'static>> {
            // 顺序校验，任一失败立即返回
            self.check_ip(ip, perm.ip_rpm)?;
            self.check_user(&perm.effective_user_id, perm.rpm_limit, perm.tpm_limit)?;
            for gid in perm.user_group_ids() { self.check_group(gid, ...)?; }
            self.check_apikey(...)?;
            Ok(RateLimitTicket { /* RAII，释放时记录消耗 */ })
        }

        pub fn check_stream_concurrency(&self, uid: &UserId, max: u32) -> Result<StreamSlot, ErrorIR<'static>> {
            // 检查 + 占位
        }
    }
    ```

- **限流响应头**（FRD §5.4 + §11.6）：
  - 429 响应必含：
    ```
    X-RateLimit-Limit: <N>            # 触发的最严层的 limit
    X-RateLimit-Remaining: 0
    X-RateLimit-Reset: <unix_ts>      # 窗口结束时间
    Retry-After: <seconds>
    X-RateLimit-Scope: user|group|key|ip   # 哪一层触发的
    ```
  - 2xx 响应可选携带 `X-RateLimit-Remaining` / `X-RateLimit-Limit` 提示客户端当前余量（仅 effective_user 层，不暴露 group/key 层避免信息泄露）。
  - 429 响应延迟预算：≤ 10ms（不穿透到上游，FRD §11.6）。

- **TPM 计费侧统计**：
  - 限流计数与计费分离：`TPM` 消耗在 logging phase 根据实际 `usage.total_tokens` 累加；`request_filter` 阶段先用 `estimate_input_tokens` 做预检（避免大请求穿透）。
  - 实现：`RateLimitTicket` 在 RAII drop 时调用 `record_consumption(actual_tokens)`，把实际消耗记入本地 TPM 桶。

- **并发流式上限**（FRD §5.5）：
  - request_filter 阶段：`is_stream && per_user_streams.entry(uid).fetch_add(1) >= max` → 立即 `fetch_sub(1)` + `GW_QUOTA_CONCURRENT_EXCEEDED`（429）+ `Retry-After: 5`。
  - 全局上限同理用 `total_streams: AtomicUsize`。
  - 流式结束（logging / drop / fail_to_proxy）→ `fetch_sub(1)`，由 `StreamSlot` RAII 保证。

- **跨实例限流的边界**：
  - 当前阶段单 pod 限流（用户 RPM/TPM 在本地令牌桶）+ Redis 周期同步（60s 窗口）；
  - 多 pod 场景下偏差上限 = `pod 数 × 单 pod 桶速率 × 同步周期`；FRD §5.4 接受 1% 误差。
  - 严格全局限流（如金融场景）：未来引入 Redis Lua 原子扣令牌（重评信号）。

- **实现位置（对齐 ADR-018）**：
  - `gw-auth`：`RateLimitGuard` + `check_request` + `check_stream_concurrency`
  - `gw-proxy`：在 `request_filter` 调 `check_request`，`is_stream` 时再调 `check_stream_concurrency`；构造响应头时写 `X-RateLimit-*`
  - `gw-store`：周期性 flush 本地 ratelimit 计数到 Redis counter（在后台 task 中由 ledger crate 同进程触发，或在 gw-auth 内自管）

- **业务规则**：
  - 限流不计费、不消耗用户余额、不增加 RPM 计数（429 本身就是 RPM 触发后才返回的）。
  - 单请求 hard cap 校验失败不消耗用户配额（FRD §5.6 业务规则）。
  - IP 黑名单（FRD §11.1）独立于限流，在 `early_request_filter` 最早执行，拒绝优先于限流。

- **验收标准**：
  - AC-024-1：用户 RPM=100，连发 101 个请求 → 第 101 个返回 429 `GW_QUOTA_RPM_EXCEEDED`，含 `X-RateLimit-*` 与 `Retry-After`。
  - AC-024-2：用户层 RPM=200，分组层 RPM=100 → 实际生效 100（min）；429 响应头 `X-RateLimit-Scope: group`。
  - AC-024-3：用户并发流式 100 → 第 101 个流式请求 429 `GW_QUOTA_CONCURRENT_EXCEEDED`。
  - AC-024-4：4M token 输入请求（`runtime.max_input_tokens=2_000_000` 默认值）→ 413 `GW_REQUEST_INPUT_TOO_LARGE`。
  - AC-024-5：429 响应延迟 P99 ≤ 10ms。
  - AC-024-6：限流计数误差 ≤ 1%（10 pod × 1 小时 benchmark）。

- **备选 A（已采用）**：本地令牌桶 + Redis 周期同步（governor 实现）。
- **备选 B**：纯 Redis Lua 原子限流。**否决**：每请求至少 1 次 Redis RTT（0.5ms）+ Redis 单核 CPU 风险（2k+ QPS 极限）；本地令牌桶 + 1% 误差是更平衡的方案。
- **备选 C**：仅本地令牌桶，不同步 Redis。**否决**：多 pod 时偏差线性放大（10 pod × 100 RPM = 实际 1000 RPM），无法满足 1% 误差目标。
- **未来重评信号**：
  - 单用户严格全局限流（金融 / 监管要求）→ 切 Redis Lua 原子扣；
  - 偏差实测超过 5% → 缩短 flush 周期到 10s 或引入 watermark 提前回拉；
  - 限流计数器存储压力大 → 把 ratelimit counter 与 billing counter 拆 Redis 实例。

### ADR-025 安全风控扩展点（新增 / v1 仅 IP 黑白名单与签名）

> **驱动需求**：FRD §11（安全与风控）。v1 实现强需求项（IP 黑白名单、请求签名），P1/P2 项（内容审计、敏感词、异常流量检测、防盗链）通过扩展点预留，**不在 v1 创建独立 crate**（对齐 ADR-018 的"v1 不创建边缘 crate"原则）。

- **决定**：
  - **v1 强制实现的安全能力**：

    | 能力 | 数据来源 | 实现位置 | FRD |
    |---|---|---|---|
    | IP 黑名单 | `entity:ipblock:<key_or_global>` | `gw-auth::ip_filter` | §11.1 |
    | IP 白名单（全局/用户/Key 三层）| `entity:user.ip_allowlist` / `entity:apikey.ip_allowlist` / `entity:vendor_key.ip_allowlist` | `gw-auth::ip_filter` | §11.1 / §9.4 |
    | 请求签名校验（HMAC-SHA256） | API Key 的 secret 字段（解密后）| `gw-auth::signature` | §11.2 |
    | 异常流量自动封禁 | 写入 `entity:ipblock` | `gw-auth` 内异步 task | §11.5（仅触发自动封禁部分） |

  - **数据结构**（`gw-core`）：
    ```rust
    pub struct IpBlockEntry {
        pub cidr: IpCidr,                // IPv4 / IPv6
        pub scope: IpBlockScope,         // Global | User(UserId) | ApiKey(ApiKeyId)
        pub reason: IpBlockReason,       // Manual | AbnormalTraffic | Compliance
        pub expires_at: Option<DateTime<Utc>>,  // None = 永久
    }

    pub struct SignatureSpec {
        pub algorithm: SignatureAlgo,    // HmacSha256
        pub headers: Vec<String>,         // 必含: X-Signature-Timestamp / X-Signature
        pub timestamp_skew_secs: u32,     // 默认 300（5 min）
    }
    ```

  - **校验顺序（最早拒绝 → 最晚校验）**：
    1. `early_request_filter`：IP 黑名单（FRD §11.1，拒绝优先于一切）→ 失败 403 `GW_AUTH_IP_BLOCKED`
    2. `request_filter`：JWT/APIKey 鉴权（ADR-023）
    3. IP 白名单（按 API Key / 用户 / Vendor Key 的 ip_allowlist 三层取交集，空 = 不限）→ 失败 403 `GW_AUTH_IP_NOT_ALLOWED`
    4. 签名校验（per-Key 开关，默认关闭，FRD §11.2）→ 失败 401 `GW_AUTH_SIGNATURE_INVALID`（不暴露具体失败原因，防信息泄露）
    5. ADR-024 限流与 hard cap
    6. ADR-023 权限合并

  - **签名算法（FRD §11.2）**：
    ```
    signing_string = HTTP_METHOD + "\n"
                   + PATH         + "\n"
                   + TIMESTAMP    + "\n"
                   + SHA256_HEX(BODY)

    expected = HMAC_SHA256(api_key_secret, signing_string)
    received = headers["X-Signature"]
    valid    = constant_time_eq(expected, received)
            && abs(now - TIMESTAMP) <= timestamp_skew_secs
    ```
    校验耗时预算 ≤ 5ms（FRD §11.2）。

  - **异常流量自动封禁（FRD §11.5 子集）**：
    - 由 `gw-auth` 后台 task 每 30s 扫描 metrics：单 IP QPS / 错误率；
    - 偏离基线（基线学习 7 天）10 倍 → 自动 `XADD events:entity ... ns=ipblock op=upsert ...` 写入临时黑名单（`expires_at = now + 1h`，可配）+ `XADD alerts severity=critical`；
    - v1 不实现"基线学习"完整算法，先用固定阈值（QPS > 100 持续 1 min）兜底；完整基线学习留待 v2。

- **v1 不实现的能力（扩展点预留）**：

  | 能力 | 扩展点 | v1 行为 | FRD |
  |---|---|---|---|
  | 内容审计 | `gw-core::trait ContentFilter` | filter 槽未挂实现，所有内容透传 | §11.3 |
  | 敏感词输入侧检测 | 同上（request body filter 调用点）| 不调用 | §11.4 |
  | 敏感词输出侧检测 | 同上（response body filter 调用点）| 不调用 | §11.4 |
  | 防盗链 Referer | `gw-auth::referer_check`（trait + 默认 noop impl） | 默认关闭 | §11.7 |
  | 完整异常流量基线学习 | 上述简化版替代 | 固定阈值 | §11.5 |
  | 单 IP 跨用户请求数检测 | metrics 暴露相关指标 | 仅 metrics，无自动处置 | §11.5 |

  扩展点 trait 定义（`gw-core`）：
  ```rust
  /// 内容审计扩展点（v1 不实现，trait 与 noop impl 都在 gw-core）
  pub trait ContentFilter: Send + Sync {
      fn check_request(&self, ir: &RequestIR<'_>) -> ContentVerdict;
      fn check_stream_chunk(&self, chunk: &StreamEventIR<'_>) -> ContentVerdict;
      fn check_response(&self, ir: &ResponseIR<'_>) -> ContentVerdict;
  }

  pub enum ContentVerdict {
      Pass,
      Tag(String),           // 仅标记，记日志（low/medium 风险）
      Reject(ErrorIR<'static>), // 拦截（high 风险）
  }

  pub struct NoopContentFilter;
  impl ContentFilter for NoopContentFilter {
      fn check_request(&self, _: &RequestIR<'_>) -> ContentVerdict { ContentVerdict::Pass }
      fn check_stream_chunk(&self, _: &StreamEventIR<'_>) -> ContentVerdict { ContentVerdict::Pass }
      fn check_response(&self, _: &ResponseIR<'_>) -> ContentVerdict { ContentVerdict::Pass }
  }
  ```

  `gw-proxy` 的 request_filter / response_body_filter 在固定位置调用 `ContentFilter`；v1 装配的实现是 `NoopContentFilter`，未来切换 = 装配另一个实现（不需新建 crate）。

- **实现位置（对齐 ADR-018）**：
  - `gw-core`：`IpBlockEntry` / `SignatureSpec` / `ContentFilter` trait + `NoopContentFilter`
  - `gw-store`：`entity:ipblock` 懒加载 + Patricia Trie 内存索引（Moka 缓存）
  - `gw-auth`：`ip_filter::check` / `signature::verify` / 异常流量后台 task / `referer_check`（noop）
  - `gw-proxy`：在 `early_request_filter` 与 `request_filter` 各 phase 串接调用

- **业务规则**：
  - IP 黑名单优先于白名单（拒绝优先于允许，FRD §11.1）。
  - 黑名单 / 白名单变更通过 `events:entity` 推送，网关 30s / 10s 内生效（FRD §11.1）。
  - 签名 per-Key 开关（默认关闭，向后兼容）；启用后签名失败的请求不计费、不计入 RPM。
  - 自动封禁写入 `entity:ipblock` 带 `expires_at`，到期后 entity TTL 自然清理。

- **验收标准**：
  - AC-025-1：客户端 IP 在黑名单中 → 早期 `early_request_filter` 阶段直接 403，不进入后续 phase。
  - AC-025-2：API Key 启用签名，签名错误 → 401（不暴露"timestamp 漂移"vs"签名不匹配"具体原因）。
  - AC-025-3：单 IP QPS > 100 持续 1 min → 自动 XADD events:entity 写入 `entity:ipblock:<ip>` 带 `expires_at = now + 3600s`，下一个网关实例 30s 内感知并拒绝。
  - AC-025-4：`NoopContentFilter` 装配下，所有请求 / chunk / 响应原样透传，无任何审计行为。

- **备选 A（已采用）**：v1 仅 IP 黑白名单 + 签名 + 自动封禁简化版；其余通过 trait 扩展点预留。
- **备选 B**：v1 完整实现 FRD §11.3-§11.4 内容审计与敏感词。**否决**：边缘能力（P1 但属 nice-to-have），实现成本高（敏感词库维护、检测算法、流式逐 chunk 检测的状态机），v1 不必为它创建 crate；满足 FRD §11.4 P1 优先级的窗口期在 v1 之后。
- **备选 C**：把异常流量检测做成独立 `gw-security` crate。**否决**：v1 简化版仅 ~100 行代码 + 1 个后台 task，属 `gw-auth` 关注点的延伸；独立 crate 无收益。
- **未来重评信号**：
  - 监管要求内容审计（FRD §11.3-11.4 升级为 P0）→ 实现 `RegexContentFilter` 或接入第三方审计 API，装配到 ContentFilter 槽位；
  - 异常流量真实场景出现 → 完善基线学习算法（v1 固定阈值替换为 ML 模型）。

### ADR-026 双级日志 + 写入时脱敏 + 响应头契约（新增）

> **驱动需求**：FRD §12.2（双级日志写入）+ §12.3（写入时脱敏）+ §12.5（trace 透传）+ ADR-020 错误响应头。

- **决定**：
  - **双级日志（FRD §12.2）**：
    | 级别 | 用途 | 保留期 | 写出方式 | 字段量 |
    |---|---|---|---|---|
    | `simple_log` | 短期热查询（账单 / 审计 / 排错）| 365 天 | XADD `billing:records` Stream，由管理面持久化到 PgSQL 后裁剪 | ~15 字段 |
    | `detailed_log` | 调试 / 性能分析 | 7 天 | 异步写到对象存储（S3 / OSS），按 1 小时分文件、gzip | ~50 字段 + request/response body sample |

  - **simple_log schema（每请求 1 条，写 `billing:records` stream）**：
    ```json
    {
      "ts_ms": 1716000000000,
      "request_id": "req_abc123",
      "trace_id": "00-...-...-01",
      "user_id": "u123",
      "child_id": "u123_sub1",          // 子账户场景才有
      "api_key_id": "ak_xxx",
      "client_protocol": "openai",
      "upstream_protocol": "anthropic",
      "model": "claude-3-opus",
      "vendor_id": "anthropic",
      "vendor_key_id": "vk_xxx",
      "status": "ok|err|cancelled",
      "error_code": "GW_UPSTREAM_5XX",  // status=err 时存在
      "http_status": 200,
      "input_tokens": 1234,
      "output_tokens": 567,
      "cached_tokens": 0,
      "cost_micro": 1500,
      "freeze_amount_micro": 2000,       // 估算冻结金额
      "markup_product": 1.2,
      "usage_source": "upstream",        // upstream | estimated | fallback_conservative
      "retry_count": 0,
      "latency_ms": 1234,
      "ttft_ms": 50,                     // 流式才有，首 token 延迟
      "is_stream": true
    }
    ```

  - **detailed_log schema（额外字段，仅写对象存储）**：
    ```json
    {
      // ... 包含 simple_log 的所有字段
      "request_body_sample": "{...}",   // 截断到 4KB，脱敏后
      "response_body_sample": "{...}",  // 同上
      "request_headers": { /* 脱敏 */ },
      "response_headers": { /* 脱敏 */ },
      "stream_chunks_count": 42,
      "retry_details": [ { "key_id": "vk_xxx", "error": "GW_UPSTREAM_TIMEOUT", "attempt_ms": 1000 } ],
      "phase_durations": {
        "auth_us": 50,
        "permission_merge_us": 30,
        "candidates_us": 100,
        "strategy_pick_us": 20,
        "estimate_freeze_us": 10,
        "upstream_connect_ms": 5,
        "upstream_ttft_ms": 50,
        "upstream_total_ms": 1200
      }
    }
    ```

  - **写入时脱敏规则（FRD §12.3）** — 在日志构造阶段执行，确保**敏感数据永不进入持久化层**：
    | 字段 | 脱敏方式 |
    |---|---|
    | `api_key`（原始上游 Key 明文）| 永不出现在任何日志（被 `SecretString` 类型系统屏蔽，ADR-017）|
    | API Key 字段（用户 API Key）| 仅显示前 4 + 后 4，中间 `**`（如 `sk-1a**3f`）|
    | JWT token | 永不出现（仅记 `kid` + `sub`）|
    | request_body / response_body | 关键词正则替换（如 `"password"`、`"secret"`、`"token"` 后的 value 替换为 `[REDACTED]`）；长度截断到 4KB |
    | Authorization 头 | `Bearer [REDACTED]` |
    | 上游响应中的 PII（邮箱、手机号、身份证）| 正则脱敏，规则可配 |
    | 用户 IP | simple_log 保留完整 IP；detailed_log 可选 hash（合规场景启用）|

  - **响应头契约**（汇总 ADR-009 / ADR-019 / ADR-020 / ADR-024 已分散的契约）：

    | 头 | 来源 | 必含 / 可选 | 内容 |
    |---|---|---|---|
    | `X-Request-Id` | 网关生成（W3C traceparent 的 trace-id 部分）| **必含** | 每请求唯一 ID，与 simple_log.request_id 对应 |
    | `X-Trace-Id` | W3C traceparent 透传 | **必含**（成功 + 失败） | 完整 traceparent |
    | `X-Upstream-Vendor` | 路由实际命中的 Vendor | 可选（默认开启） | `openai` / `anthropic` / `gemini` / 自定义 |
    | `X-Upstream-Key-Id` | 命中的 vendor_key_id | 可选（默认关闭，避免泄露内部 Key id）| `vk_xxx` |
    | `X-RateLimit-Limit` | 用户 effective RPM | 限流 429 时**必含**；2xx 可选 | 整数 |
    | `X-RateLimit-Remaining` | 当前余量 | 同上 | 整数 |
    | `X-RateLimit-Reset` | 窗口结束 unix_ts | 同上 | 整数 |
    | `X-RateLimit-Scope` | 触发的限流层 | 429 时**必含** | `user` / `group` / `key` / `ip` |
    | `Retry-After` | 建议重试间隔（秒）| 429 / 503 时**必含** | 整数 |
    | `X-Cost-Micro` | 本请求实际扣费 | 2xx 可选（默认关闭，避免泄露内部计费）| 整数 |
    | `X-Gateway-Version` | 网关版本号 | 必含 | semver |

- **写出路径**：
  - **simple_log**：在 `gw-proxy::logging` phase 由 `gw-ledger` 的 mpsc 接管，批量 flush 到 `billing:records` Stream（ADR-004 + ADR-013 协议 3）。Stream 由 Go 管理面 XREAD 落库到 PgSQL，PgSQL 端做 365 天保留。
  - **detailed_log**：在 `gw-proxy::logging` phase 由独立的 `detailed_log_writer` task 异步写到对象存储；批量按 1 小时分文件 + gzip，使用 multipart upload。失败重试 3 次，最终失败仅记 metrics 不阻塞响应。
  - **错误日志（tracing）**：与上述持久化日志独立，由 `tracing-subscriber` 输出到 stderr 的 JSON line；包含 panic / 业务异常 / cold-path 失败等运维关注信息。

- **trace 透传（FRD §12.5）**：
  - 客户端在请求头携带 `traceparent: 00-<trace-id>-<parent-id>-<flags>` 时，网关解析后挂到 `ReqCtx.trace_id`，向上游发请求时**原样透传**该头。
  - 客户端未携带时，网关生成新 trace-id（基于 ULID）+ 新 parent-id。
  - 所有 tracing span 都挂载到 `trace_id`；OTLP exporter 可选。

- **实现位置（对齐 ADR-018）**：
  - `gw-core`：`SimpleLogRecord` / `DetailedLogRecord` 类型 + 脱敏 trait（`pub trait Redactor { fn redact(&self, s: &mut String); }`）+ 默认 `DefaultRedactor` impl
  - `gw-ledger`：simple_log 的 mpsc + flush（已在 ADR-004 描述）
  - `gw-proxy`：`logging` phase 编排两级日志的字段填充 + 调用 `Redactor` + 写出
  - `gw-gateway`（bin）：对象存储 client 装配（S3 / OSS SDK）

- **业务规则**：
  - **simple_log 永不丢失关键字段**：even when usage 缺失，`usage_source` 必填（`fallback_conservative`）；权利方对账依赖此字段。
  - detailed_log 可丢失（不阻塞响应、不重试 > 3 次）；只是调试能力降级。
  - 脱敏在**构造日志结构时**执行，不能依赖下游 writer 脱敏（防止"忘记接 redactor"的运维事故）。
  - 客户端断开（`Cancelled`）的请求**仍写 simple_log**（`status=cancelled`），便于审计；按已输出 token 计费（ADR-004 失败矩阵）。

- **验收标准**：
  - AC-026-1：每请求恰好写 1 条 simple_log（成功 / 失败 / 取消都写）。
  - AC-026-2：detailed_log 写失败不影响 2xx 响应正常返回；metrics `detailed_log_write_failures_total` 可见。
  - AC-026-3：simple_log / detailed_log 中均无明文上游 api_key（grep 验证）。
  - AC-026-4：请求头携带 `traceparent` 时上游收到的请求头中 `traceparent` 值不变。
  - AC-026-5：429 响应必含 `X-RateLimit-*` 与 `Retry-After`、`X-RateLimit-Scope`。

- **备选 A（已采用）**：双级日志（simple / detailed）+ 写入时脱敏。
- **备选 B**：单一日志层（仅 detailed_log）。**否决**：detailed_log 含 4KB body sample，账单查询 365 天会占用大量对象存储 + 查询成本高；FRD §12.2 明确双级。
- **备选 C**：把脱敏放在 Go 落库阶段。**否决**：违反"敏感数据零暴露"原则（Redis Stream 是中间层，泄漏概率更大）；网关侧脱敏是唯一防线。
- **未来重评信号**：
  - 日志体积压力大（单日 > 1TB）→ detailed_log 改为采样写出（如 1% 抽样）；
  - 合规要求"完整请求体留存" → 引入独立的合规存储（加密、access log、保留 N 年）。

### ADR-027 Debug 模式 = 请求级开关 + 全链路时间/内存追踪 + 详细日志（新增）

> **驱动需求**：性能瓶颈定位与生产问题诊断。需要在不影响主流量的前提下，对**特定请求**触发完整 phase 级时间统计、内存增量统计、最详细的日志输出（保留脱敏约束），便于事后回放性能问题。
>
> **核心设计原则**：**调试模式必须能反映每一个 phase 的性能**——即便 debug 触发条件在请求中段才确定（如基于 user entity 的 `debug_enabled` flag），早期 phase（early_request_filter / auth / IP 校验）的 timing 与 memory delta 也**不能丢失**。这通过"always-on 预录"机制实现。

- **决定**：
  - 引入"调试请求"的概念：单个请求被标记为 `debug = true` 后，整个生命周期所有 phase 的 timing + memory delta + 详细字段日志会被序列化输出到独立 sink。
  - **绝不**全局启用 debug 日志输出（写出开销不可接受）；但**记录**是 always-on 的（开销极低，见下文）。
  - 通过四种受控机制开启**输出**：

    | 开启方式 | 决策时机 | 用途 | 控制位置 |
    |---|---|---|---|
    | 请求头 `X-Debug-Trace: <token>` | `early_request_filter` 入口 | 单请求精准触发 | `gw-proxy::early_request_filter`；token 必须匹配 `config.debug.access_token`，否则忽略（避免被外部滥用）|
    | 全局采样 `config.debug.sampling_rate` | `early_request_filter` 入口 | 持续低强度采样（默认 0.0001 = 万分之一） | `early_request_filter`，`rand::random::<f32>() < rate` |
    | trace_id 末位匹配 `config.debug.trace_id_suffix` | `early_request_filter` 入口 | 紧急排障时不改部署快速开启 | `early_request_filter` |
    | per-user `entity:user.debug_enabled = true` | `request_filter`（user 加载后）| 内部测试账号长期开启 | `gw-auth::merge_permissions` 阶段读取并 commit 到 `ReqCtx.debug` |

- **Always-on 预录机制**（解决"per-user 触发会丢早期 phase"的问题）：

  ```rust
  pub struct ReqCtx {
      /* ... */
      pub trace: TraceBuffer,          // 永远存在，零开销恒常打开
      pub debug: DebugMode,             // emit 决策；early_request_filter 给初值，request_filter 可上升
  }

  /// 固定容量栈分配缓冲（不 heap alloc），始终记录 phase 时间与内存
  pub struct TraceBuffer {
      phases: [PhaseRecord; 32],        // ≥ ADR-027 定义的 20 个固定 phase + 余量
      phase_count: u8,
      stream_chunks: Vec<ChunkRecord>,   // debug=true 决策后才开始填充（不限制采样率）
      ir_mutations: Vec<MutationRecord>, // 同上
      filter_drop_reasons: SmallVec<[(KeyId, &'static str); 8]>,
  }

  pub struct PhaseRecord {
      pub name: &'static str,           // 'static 标签，零分配
      pub start_ns: u64,                 // process-relative ns（Instant::now 转换）
      pub end_ns: u64,                   // 0 表示进行中
      pub alloc_at_start: u64,           // jemalloc thread.allocated 快照
      pub alloc_at_end: u64,
  }
  ```

  **核心要点**：
  - `phases` 数组在 `ReqCtx` 内**栈分配**（固定 32×32=1KB），不 heap alloc；
  - 每次 `phase_start` / `phase_end` = 一次数组下标写入 + `Instant::now` (~3ns) + `jemalloc_ctl::thread::allocated::read` (~10ns，TLS cache hit) = **总约 15-20ns/phase 边界**；
  - 20 个 phase 边界 × 20ns ≈ **400ns/请求恒常开销**，占 200µs P50 预算的 0.2%，可接受；
  - **不论后续 debug 决策如何**，所有 phase 都被记录；如果 debug=false → 整个 buffer 在 `ReqCtx::drop` 时随栈一起释放，**零额外成本**；
  - 如果 debug=true → logging phase 序列化整个 buffer 到 detailed_log + debug sink。
  - `stream_chunks` / `ir_mutations` / `filter_drop_reasons` 是 `Vec` / `SmallVec`，只有在 `ReqCtx.debug.enabled == true` 后才填充（普通请求不分配）。

- **DebugMode 决策结构**：

  ```rust
  pub struct DebugMode {
      pub enabled: bool,                    // false 时不写出 trace，但 TraceBuffer 仍在记录
      pub trigger: DebugTrigger,             // Header | UserFlag | Sampling | TraceSuffix | Disabled
      pub include_request_body: bool,        // 默认 true
      pub include_response_body: bool,       // 默认 true
      // 不限制流式 chunk 日志率（用户要求："debug 模式仅用于调试，不限制"）
  }
  ```

  决策时序：
  1. **`early_request_filter` 入口**：评估 header / sampling / trace_suffix 三个触发条件；命中则 `ReqCtx.debug = DebugMode { enabled: true, trigger: ... }` 并立刻分配 `stream_chunks` / `ir_mutations` 等 Vec；
  2. **`request_filter` 中（user entity 加载后）**：如果 `merged_perm.user.debug_enabled == true` 且当前 `ReqCtx.debug.enabled == false` → "升级"为 enabled=true（同时分配 Vec）；
  3. **此后所有 phase**：始终向 `ReqCtx.trace.phases` 写入（无论 enabled 与否）；向 `stream_chunks` / `ir_mutations` 写入只在 enabled=true 时执行；
  4. **`logging` phase**：如果 `debug.enabled == true` → 序列化 `trace` + emit；否则 → 直接丢弃。

- **Phase 级时间追踪**（与之前保持，但记录在 always-on buffer 内）：

  追踪的固定 phase 列表（按 §5.1 时序，共 21 个）：

  | # | Phase | 起止 |
  |---|---|---|
  | 1 | `early_request_filter` | 进入 → 退出 |
  | 2 | `auth_jwt_verify` | jwt parse → user_id resolved |
  | 3 | `auth_apikey_lookup` | apikey id → entity 加载 → hash 校验 |
  | 4 | `auth_signature_verify` | signing_string 构造 → HMAC |
  | 5 | `entity_load:<ns>` | Moka get_with 起 → 返回（每个 ns 独立 phase）|
  | 6 | `codec_parse_request` | body → IR |
  | 7 | `permission_merge` | merge_permissions 起止 |
  | 8 | `ratelimit_check` | governor check_n × 各维度 |
  | 9 | `hard_cap_check` | estimate_input_tokens + cost cap |
  | 10 | `freeze_estimate` | DEDUCT.frozen.add |
  | 11 | `get_candidate_keys` | ModelGroup.filter |
  | 12 | `strategy_pick` | Strategy.pick |
  | 13 | `kms_decrypt` | per-Key 首次冷加载 |
  | 14 | `upstream_connect` | TCP/TLS 握手完成 |
  | 15 | `codec_render_request` | IR → upstream body |
  | 16 | `upstream_ttfb` | 上游首字节到达 |
  | 17 | `codec_parse_stream_chunk` | per chunk（不采样，全记）|
  | 18 | `codec_render_stream_chunk` | per chunk |
  | 19 | `freeze_settle` | actual_cost → frozen.sub + deducted.add |
  | 20 | `ledger_try_send` | mpsc try_send |
  | 21 | `detailed_log_write` | 构造 + send to writer |

- **内存追踪（jemalloc 集成，默认启用）**：
  - `gw-gateway` 在 `Cargo.toml` 引入 `tikv-jemallocator` 替换默认 allocator，启用 `stats` feature；
  - **默认启用**（生产 release build）：`#[global_allocator] static ALLOC: Jemalloc = Jemalloc;`
  - 提供工具函数 `mem::sample_thread_alloc() -> u64`：返回当前 thread 的 `jemalloc.thread.allocated`（TLS cached MIB，~10ns）；
  - 每个 phase 起止各调用一次 `mem::sample_thread_alloc()`，差值即 heap_delta；
  - **始终记录**（与 always-on phase timing 一致）：即便 debug=false，每个 phase 也记录 alloc 数据到栈缓冲；
  - 决策侧：jemalloc 默认启用让 release build 的常态内存采样可用；同时 jemalloc 自身提供 ~10-20% 的多线程 alloc 性能提升（独立收益）；
  - 工具兼容性：valgrind / heaptrack 在 staging 环境需要禁用 jemalloc → 通过 `--no-default-features` build 一个备用二进制。

- **详细日志字段（DebugLogRecord）**：

  扩展 ADR-026 的 `DetailedLogRecord`，debug=true 时额外写入：

  ```json
  {
    /* ... 包含 ADR-026 detailed_log 的所有字段 ... */
    "debug": {
      "trigger": "header|user_flag|sampling|trace_suffix",
      "trigger_decided_at_phase": "early_request_filter|request_filter",  // 标记触发点
      "phases": [
        {
          "name": "early_request_filter",
          "start_us": 0,
          "duration_us": 8,
          "heap_delta_bytes": 64,
          "events": []
        },
        {
          "name": "auth_jwt_verify",
          "start_us": 12,
          "duration_us": 45,
          "heap_delta_bytes": 256,
          "events": [
            { "at_us": 30, "kind": "cache_lookup", "detail": "jwks:kid_abc hit_moka" }
          ]
        },
        { "name": "entity_load:user", "start_us": 60, "duration_us": 1200, "heap_delta_bytes": 4096,
          "events": [{ "kind": "cache_lookup", "detail": "user:u123 miss_moka -> hit_redis 850us" }] },
        { "name": "codec_parse_request", "start_us": 1300, "duration_us": 187, "heap_delta_bytes": 1280 },
        /* ... 一直到 logging */
      ],
      "ir_snapshot": { /* RequestIR 字段（脱敏后） */ },
      "ir_mutations": [
        { "field": "max_tokens", "from": 100000, "to": 4096, "reason": "group_max_tokens_cap" }
      ],
      "candidates_before_filter": ["vk_1","vk_2","vk_3","vk_4"],
      "candidates_after_filter": ["vk_1","vk_3"],
      "filter_drop_reasons": {
        "vk_2": "rate_limited",
        "vk_4": "in_flight_full"
      },
      "strategy_score_table": [
        { "key_id": "vk_1", "score": 0.32, "components": { "in_flight": 0.1, "latency_ms": 80, "error_rate": 0.0 } },
        { "key_id": "vk_3", "score": 0.65, "components": { "in_flight": 0.4, "latency_ms": 95, "error_rate": 0.05 } }
      ],
      "retry_attempts": [
        { "attempt": 1, "key_id": "vk_1", "error": "GW_UPSTREAM_TIMEOUT", "elapsed_ms": 30000 },
        { "attempt": 2, "key_id": "vk_3", "result": "ok" }
      ],
      "stream_chunks": [          // 全量记录，不采样（用户要求："debug 模式仅用于调试，不限制"）
        { "idx": 0,   "at_us": 50100,  "size_bytes": 512, "parse_duration_ns": 4200, "render_duration_ns": 0,    "parsed_event": "Start" },
        { "idx": 1,   "at_us": 50250,  "size_bytes": 480, "parse_duration_ns": 3900, "render_duration_ns": 0,    "parsed_event": "Delta" },
        { "idx": 2,   "at_us": 50410,  "size_bytes": 510, "parse_duration_ns": 4100, "render_duration_ns": 4500, "parsed_event": "Delta" },
        /* ... 全部 N 个 chunk */
        { "idx": 499, "at_us": 132000, "size_bytes": 88,  "parse_duration_ns": 1200, "render_duration_ns": 0,    "parsed_event": "End" }
      ],
      "memory_peak_bytes": 32768,
      "memory_total_alloc_bytes": 65536,
      "thread_id": "tokio-worker-12"
    }
  }
  ```

- **脱敏约束（与 ADR-026 一致，debug 不放松）**：
  - `api_key` 明文**永远**不出现在 debug log 中（`SecretString` 类型系统屏蔽）；
  - JWT body 不出现；仅记 `kid` / `sub` / `iss`；
  - `request_body_sample` / `response_body_sample` 在 debug 模式下仍按 Redactor 规则脱敏（替换 `password` / `secret` / `token` 的 value）；
  - debug 模式**不能**用于绕过脱敏；这是不可关闭的约束。

- **输出 sink**：
  - debug 日志写入与 `detailed_log` 同一对象存储 bucket，但前缀 `debug-trace/<date>/<hour>/`，便于运维筛选；
  - 同时通过 `tracing` 的独立 target `gw_debug_trace` 输出到 stderr（JSON line），便于本地调试快速定位；
  - **不**写入 `billing:records`（debug 数据不参与对账）；
  - 写出失败仅 metric `gateway_debug_trace_write_failures_total`，不影响响应。

- **保护机制**：
  - `config.debug.max_concurrent_traces`（默认 100）：同时进行的 debug 请求**输出**上限（不是记录上限——记录是 always-on）；超出则后到的请求 `debug.enabled = false`，trace buffer 仍记录但不写出；
  - `config.debug.global_kill_switch`（默认 false）：紧急时 set true → 所有 debug 触发立即降级为 enabled=false（记录仍发生，仅不写出），无需重启；
  - `config.debug.access_token` 留空时禁用 `X-Debug-Trace` 头机制（默认留空，必须运维显式配置）。

- **性能开销估算（更新）**：
  - **常态请求（debug=false）**：
    - 21 个 phase 边界 × (3ns Instant + 10ns jemalloc sample + 5ns 数组写入) = **~380ns/请求**
    - 占 200µs P50 预算的 0.19%
    - 内存：固定 1KB 栈分配，无 heap
  - **debug=true 请求**：
    - 上述基础开销 + 流式 chunk 全记录（每 chunk ~30ns 记录 + Vec push）
    - 详细日志构造与序列化：~50µs（字段聚合 + JSON serialize + 异步 send）
    - 假设 500 chunk 的流式请求：500 × 30ns = 15µs chunk 记录开销
    - 单 debug 请求总开销 ≈ 50-100µs + chunk-overhead
  - **系统级**：万分之一采样 + 2k QPS = 0.2 req/s 全量 debug，对系统几乎无影响；即便 1% 采样 = 20 req/s，CPU 占用 < 0.1%。

- **实现位置（对齐 ADR-018 9-crate DAG）**：
  - `gw-core`：`TraceBuffer` / `PhaseRecord` / `DebugMode` / `TraceEvent` 类型 + `pub trait DebugSink { fn emit(&self, trace: DebugTrace); }` + `NoopDebugSink` + `mem::sample_thread_alloc()`
  - `gw-store`：所有 entity 加载链 + counter 操作内调用 `ctx.trace.phase_start("entity_load:user")` / `phase_end()` 钩子，恒常打开
  - `gw-auth` / `gw-routing` / `gw-ledger`：各 phase 边界调 `ctx.trace.phase_start/end`；`gw-auth` 中读到 `user.debug_enabled` 时调用 `ctx.debug.upgrade_to_enabled(DebugTrigger::UserFlag)`
  - `gw-codec`：codec 入口 / 出口（在 trait 方法的手写包装层中）添加追踪点；derive 宏生成的内部代码**不**插入追踪点（保持热路径不变）
  - `gw-proxy`：`early_request_filter` 三种触发条件评估 + `ReqCtx::new(trace, debug)` 初始化；`logging` phase 决定是否 emit
  - `gw-gateway`：装配 `tikv-jemallocator` 全局 allocator（默认启用） + `tracing` subscriber 的 `gw_debug_trace` target 路由

- **追踪点 API 草稿（`gw-core::debug`）**：
  ```rust
  impl TraceBuffer {
      #[inline(always)]
      pub fn phase_start(&mut self, name: &'static str) {
          // always-on：debug=false 也记录
          let idx = self.phase_count as usize;
          if idx >= self.phases.len() {
              return;                          // 静默截断（不应发生，phase 数有上限）
          }
          self.phases[idx] = PhaseRecord {
              name,
              start_ns: now_ns(),               // ~3ns
              end_ns: 0,
              alloc_at_start: mem::sample_thread_alloc(),  // ~10ns
              alloc_at_end: 0,
          };
          self.phase_count += 1;
      }

      #[inline(always)]
      pub fn phase_end(&mut self) {
          let idx = (self.phase_count as usize).saturating_sub(1);
          if idx >= self.phases.len() { return; }
          let p = &mut self.phases[idx];
          p.end_ns = now_ns();
          p.alloc_at_end = mem::sample_thread_alloc();
      }

      #[inline(always)]
      pub fn chunk(&mut self, debug: &DebugMode, idx: u32, size: u32, parse_ns: u64, render_ns: u64) {
          if !debug.enabled { return; }                    // chunk 记录仅 enabled 时
          self.stream_chunks.push(ChunkRecord { idx, size, parse_ns, render_ns, at_ns: now_ns() });
      }

      #[inline(always)]
      pub fn ir_mutation(&mut self, debug: &DebugMode, field: &'static str, reason: &'static str) {
          if !debug.enabled { return; }
          self.ir_mutations.push(MutationRecord { field, reason, at_ns: now_ns() });
      }
  }

  impl DebugMode {
      /// 在 request_filter 中 user entity 加载后调用，根据 user.debug_enabled 决定是否升级
      #[inline]
      pub fn upgrade_to_enabled(&mut self, trigger: DebugTrigger) {
          if self.enabled { return; }
          self.enabled = true;
          self.trigger = trigger;
          // Vec 容器在此处惰性分配（chunk / ir_mutations）
      }
  }
  ```

- **业务规则**：
  - debug 模式**仅观测、不改变行为**：debug=true 不影响路由决策、不影响计费、不放过限流、不影响重试；
  - debug log 不参与对账，不计入 `billing:records`；
  - debug header `X-Debug-Trace` 必须配合 `access_token` 使用，token 不匹配则视为无 debug 头（不返回错误，避免泄露开关存在）；
  - **always-on 预录**：phase timing 与 memory delta 从 `early_request_filter` 第一个 phase 起就被记录；后续无论 debug 是 false / 在 request_filter 升级为 true / 一直 false，对**记录**行为没有影响——这是为了保证 per-user 触发的请求也能拿到完整的早期 phase 数据；
  - per-user debug_enabled 仅用于内部测试账号，生产用户禁止该 flag（管理面 schema check）。

- **配置参数（追加到 §15 / `config.toml`）**：

  ```toml
  [debug]
  enabled = true                       # 总开关；false 则忽略所有触发机制
  access_token = ""                    # X-Debug-Trace 头校验 token；留空禁用该机制
  sampling_rate = 0.0001               # 全局采样率 0-1
  trace_id_suffix = ""                 # 紧急排障用，匹配末位则触发
  max_concurrent_traces = 100          # 同时输出 debug log 的请求数上限
  global_kill_switch = false           # 紧急关闭所有 debug 输出（记录仍持续）
  include_request_body = true
  include_response_body = true
  # 注意：无 stream_chunk_sample_rate 配置——chunk 全量记录，不采样
  sink_path_prefix = "debug-trace/"    # 对象存储路径前缀
  stderr_target = "gw_debug_trace"     # tracing target 名
  ```

- **验收标准**：
  - AC-027-1：debug=false 路径上 `cargo asm` 验证 `phase_start` / `phase_end` 不可被消除（这是 always-on 的语义保证），但每次调用恒定为 ~15-20ns（microbench）；总请求开销 ≤ 500ns。
  - AC-027-2：debug=true 单请求总开销 ≤ 100µs（不含 chunk 全记录的开销），通过 microbench 验证；500 chunk 流式请求 chunk 记录开销 ≤ 20µs。
  - AC-027-3：debug log 中 grep `api_key` / `Bearer ` 后的具体 token 字符 → 无命中（脱敏正确）。
  - AC-027-4：`X-Debug-Trace: wrong_token` 请求与无 debug 头请求行为完全一致（不开启 emit、不返回错误、不写额外日志，但 trace buffer 仍记录）。
  - AC-027-5：`max_concurrent_traces=100`，连发 200 个 debug 请求 → 后 100 个 metric `gateway_debug_traces_rejected_total{reason=concurrent}` 增加；但所有 200 个请求的 trace buffer 都在记录（验证：在 ReqCtx::drop 时统计 phase_count 都 = 21）。
  - AC-027-6：`global_kill_switch=true` 立即生效（无需重启）→ 后续所有 debug 触发被忽略（emit 端）；但内部 trace buffer 不受影响。
  - AC-027-7：**per-user trigger 完整性测试**：测试账号请求 → debug 触发在 `request_filter` 才发生 → emit 的 debug log 中 `phases[0].name == "early_request_filter"`、`phases[1].name == "auth_jwt_verify"` 等早期 phase **完整在场**，timing 数据有效。
  - AC-027-8：jemalloc 默认启用 → release build 通过 `cargo build --release` 后二进制 `nm gateway | grep jemalloc` 有匹配；valgrind 备用 build 通过 `--no-default-features` 关闭。

- **备选 A（已采用）**：always-on 预录 + 请求级 opt-in emit + 多触发源 + jemalloc 默认启用 + 流式 chunk 全量记录。
- **备选 B**：debug 决策推迟到 request_filter 才开始记录。**否决**：用户明确"不接受丢失早期 phase 性能数据"。
- **备选 C**：always-on emit（所有请求都输出 debug log）。**否决**：生产 2k+ QPS 下 emit 开销 50-100µs/请求 × 全量 = 占 25-50% CPU，不可接受；emit 必须 opt-in。
- **备选 D**：chunk 采样以减少 debug=true 时日志大小。**否决**：用户明确"不限制，debug 模式仅用于调试"。
- **备选 E**：jemalloc 选装而非默认。**否决**：用户明确"默认启用"；jemalloc 同时提供 alloc 性能提升与 stats 能力，默认启用是双收益。
- **备选 F**：用 `tracing` 现有 `Level::TRACE` 替代。**否决**：tracing level 是全局过滤；无法做 per-request 开关，也不携带 memory delta / phase breakdown 这种结构化数据。
- **备选 G**：内存追踪用 `dhat` heap profiler。**否决**：dhat 对每个 alloc 记账，开销 5-10×，只能在专门诊断 build 启用；jemalloc stats 是常态可用的轻量替代。**未来重评信号**：单请求 alloc 分布需要 byte-level 分析 → 在 staging 用 `--features=dhat` 重新 build。
- **未来重评信号**：
  - always-on 预录开销实测超过 1µs/请求 → 评估降低 phase 粒度或改为 sampling（如每 10 个请求才记忆 delta）；
  - 出现"debug 请求结果与正常请求行为不一致" → 审查是否有 debug=true 路径上意外改变了语义（违反"仅观测、不改变行为"原则）；
  - chunk 全量记录在 100K+ chunk 的超长流式请求中导致 OOM → 在 `ReqCtx::drop` 时强制 truncate `stream_chunks` 到固定上限（如 10000 条）并 alert。


---

## 4. 数据模型

> 本节是 §3 各 ADR 中提到的所有类型与协议的汇总，按"类型层 → 存储协议 → 表 schema"三段组织。所有类型按 ADR-018 的 crate 划分定位。

### 4.1 协议层 Schema（Redis 视角）

所有 Redis key 都遵循 ADR-013 定义的协议族。下表是当前业务字段到协议的映射：

| 业务资源 | 协议 | namespace / scope | id | enc | 主要字段 | 定义 ADR |
|---|---|---|---|---|---|---|
| 用户元数据 | Entity | `user` | `<user_id>` | json | parent_id / tier / status / allowed_models / rpm / tpm / max_concurrent_streams / daily_quota_tokens / user_group_ids / markup | ADR-023 |
| 用户分组 | Entity | `user_group` | `<group_id>` | json | priority / allowed_models / model_group_binding / rpm / tpm / markup | ADR-023 |
| API Key | Entity | `apikey` | `<key_id>` | json | user_id / status / hash / allowed_models / rpm / tpm / markup / ip_allowlist / expires_at | ADR-023 |
| 供应商 | Entity | `vendor` | `<vendor_id>` | json | base_urls / auth_template / timeout_overrides / tls_config / default_proxy | ADR-021 |
| Vendor Key | Entity | `vendor_key` | `<key_id>` | json | vendor_id / **ciphertext+nonce+auth_tag+kek_kid** / accessible_models / model_mapping / accessible_endpoints / protocol_conversion / priority / weight / rpm / tpm / max_usage_count / proxy / ip_allowlist / expires_at / status(7 状态机) / tags | ADR-017 + ADR-021 |
| 模型分组 | Entity | `model_group` | `<group_name>` | json | strategy / cross_vendor_fallback / max_tokens_cap / default_params / forced_params / vendor_key_ids | ADR-003 + ADR-021 |
| 计价 | Entity | `pricing` | `<model_name>` | json | input_price_micro / output_price_micro / max_output / model_markup / connect/read/total/stream_total timeouts | ADR-009 + ADR-004 |
| JWKS | Entity | `jwks` | `<kid>` | json | pem | sec §5.1 |
| IP 黑/白名单 | Entity | `ipblock` | `<cidr_or_scope_id>` | json | cidr / scope / reason / expires_at | ADR-025 |
| 用户余额 | Counter | `user` | `<user_id>` | — | field=`balance_micro` | ADR-015 |
| 当日 token | Counter | `user` | `<user_id>` | — | field=`tokens_today_<YYYYMMDD>` | ADR-004 |
| 累计 token | Counter | `user` | `<user_id>` | — | field=`tokens_total` | ADR-004 |
| Vendor Key 使用次数 | Counter | `vendor_key` | `<key_id>` | — | field=`used_count` | ADR-021 (FRD §9.6) |
| 限流计数 | Counter | `ratelimit` | `<scope>:<id>` | — | field=`rpm_<window>` / `tpm_<window>` | ADR-024 |
| 计费流水 | Append log | — | — | — | stream=`billing:records`，schema 见 ADR-026 simple_log | ADR-004 + ADR-026 |
| 异常告警 | Alert | — | — | — | stream=`alerts`，payload={ type, severity, ns?, id?, data } | ADR-013 |

### 4.2 协议结构（参考 ADR-013）

```
# Entity（懒加载源，全部 namespace 共用）
entity:<ns>:<id>             HASH    { v: <u64>, enc: "json", d: <bytes> }
entity:<ns>:<id>:version     STRING  Go 接管后 INCR（当前阶段未使用）

# Counter（懒读、双写）
counter:<scope>:<id>:<field> STRING  <int>

# Stream events
events:entity                STREAM  ns id v op enc d
events:counter               STREAM  scope id field value ts_ms

# Append log
billing:records              STREAM  Rust XADD / Go XREAD（365 天）
alerts                       STREAM  Rust XADD / Go XREAD
```

#### Stream consumer group

```
XGROUP CREATE events:entity   gateway-<instance_id> $ MKSTREAM
XGROUP CREATE events:counter  gateway-<instance_id> $ MKSTREAM
```

每个 Rust 实例使用唯一 consumer group ID（基于 hostname + pod_id），保证每实例独立消费、互不干扰。

### 4.3 内存模型（按 crate 分布）

**`gw-core` — 纯类型层（无 IO / 无 async）**：

```rust
// IR（ADR-019）
pub struct RequestIR<'a> { /* model, messages, stream, max_tokens, ... extensions, untouched */ }
pub enum   MessageIR<'a> { System, User, Assistant, Tool }
pub enum   ContentIR<'a> { Text, Parts }
pub enum   ContentPart<'a> { Text, Image, Audio, File }
pub struct ResponseIR<'a> { /* id, model, messages, finish_reason, usage, metadata */ }
pub enum   StreamEventIR<'a> { Start, Delta, Usage, End, Error }
pub struct UsageIR { input_tokens, output_tokens, cached_tokens }

// 错误层（ADR-020）
pub struct ErrorIR<'a> { category, status, code, message, retryable, ... }
pub enum   ErrorCategory { ClientBadRequest, ClientAuth, ClientQuota, UpstreamError,
                           UpstreamTimeout, UpstreamOverloaded, GatewayInternal, Cancelled }
// 稳定错误码常量
pub const GW_AUTH_INVALID_JWT: &str = "GW_AUTH_INVALID_JWT";
// ... 23 个稳定错误码

// 业务实体（ADR-021, ADR-023）
pub struct UserEntry { user_id, parent_id: Option<UserId>, tier, status,
                       allowed_models, allowed_channels, allowed_groups,
                       rpm_limit, tpm_limit, max_concurrent_streams,
                       daily_quota_tokens, user_group_ids, markup }
pub struct UserGroup { id, priority, allowed_models, allowed_channels,
                       model_group_binding, rpm, tpm, max_concurrent_streams, markup }
pub struct ApiKeyEntry { key_id, user_id, status, hash, allowed_models,
                         rpm, tpm, markup, ip_allowlist, expires_at }
pub struct Vendor { id, display_name, base_urls, auth_template,
                    timeout_overrides, tls_config, default_proxy, contact }
pub struct VendorKey { key_id, vendor_id, api_key: SecretString,
                       accessible_models, model_mapping, accessible_endpoints,
                       protocol_conversion, priority, weight,
                       rpm_limit, tpm_limit, max_usage_count, proxy,
                       ip_allowlist, expires_at, status: KeyStatus, tags }
pub enum   KeyStatus { Active, Disabled, Expired, Depleted, RateLimited, Rotated, Deleted }
pub struct ModelGroup { group, strategy: StrategyKind, cross_vendor_fallback,
                        max_tokens_cap, default_params, forced_params, vendor_key_ids }
pub struct Pricing { model, input_price_micro, output_price_micro, max_output,
                     model_markup, connect_timeout_ms, read_timeout_ms,
                     total_timeout_ms, stream_total_timeout_ms }
pub struct JwksEntry { kid, pem }
pub struct IpBlockEntry { cidr, scope, reason, expires_at }

// 共享原语
pub struct Versioned<T> { version: u64, value: Arc<T> }
pub enum   Encoding { Json, Msgpack }   // ADR-014 预留扩展位
pub struct MergedPermissions { /* ADR-023 */ }
pub struct ProtocolKind {  /* OpenAi, Anthropic, Gemini */ }

// 扩展点 trait（v1 默认 noop impl）
pub trait ContentFilter { /* ADR-025 */ }
pub struct NoopContentFilter;
pub trait TokenEstimator { fn estimate(&self, text: &str, model: &str) -> u32; }
pub struct NoopTokenEstimator;
```

**`gw-store` — 存储层**：

```rust
pub struct EntityStore {
    pub user:        moka::future::Cache<UserId,    Arc<Versioned<UserEntry>>>,
    pub user_group:  moka::future::Cache<UserGroupId, Arc<Versioned<UserGroup>>>,
    pub apikey:      moka::future::Cache<ApiKeyId,  Arc<Versioned<ApiKeyEntry>>>,
    pub vendor:      moka::future::Cache<VendorId,  Arc<Versioned<Vendor>>>,
    pub vendor_key:  moka::future::Cache<KeyId,     Arc<Versioned<VendorKey>>>,
    pub model_group: moka::future::Cache<Group,     Arc<Versioned<ModelGroup>>>,
    pub pricing:     moka::future::Cache<ModelName, Arc<Versioned<Pricing>>>,
    pub jwks:        moka::future::Cache<Kid,       Arc<Versioned<JwksEntry>>>,
    pub ipblock:     moka::future::Cache<String,    Arc<Versioned<IpBlockEntry>>>,
}

pub struct CounterStore {
    redis: Arc<fred::pool::RedisPool>,
}

// 余额扣账（Counter 协议的本地缓冲，ADR-015 三层）
pub struct DeductionState {
    pub base_micro:     AtomicI64,
    pub deducted_micro: AtomicI64,
    pub frozen_micro:   AtomicI64,
}
pub static DEDUCT: Lazy<DashMap<UserId, Arc<DeductionState>>>;

// 用户级断路（ADR-004）
pub enum UserCircuit { Closed, Open { until: Instant, reason: CircuitReason } }
pub static USER_CIRCUIT: Lazy<DashMap<UserId, UserCircuit>>;
```

**`gw-routing` — 路由层**：

```rust
// Key 级断路（ADR-008）
pub struct BreakerState {
    pub failure_window: Mutex<SlidingWindow>,
    pub circuit:        AtomicU8,                // 0=closed, 1=open, 2=half-open
    pub open_until:     AtomicU64,                // unix_ms
    pub in_flight:      AtomicU32,
}
pub struct BreakerStore { /* DashMap<KeyId, Arc<BreakerState>> */ }

pub trait Strategy { /* ADR-003 */ }
pub enum StrategyKind { CacheAffinity, FixedOrder, RoundRobin, LoadBalance,
                        CostFirst, LatencyFirst, WeightedRR, Sticky }

pub struct RetryExecutor { /* ADR-022 */ }
pub struct RetryConfig { /* mode, same_key_max, cross_key_max, ... */ }
```

**`gw-ledger` — 计费层**：

```rust
pub struct BillingRecord { /* ADR-026 simple_log schema */ }
pub struct Ledger { mpsc_tx, flusher_handle }
pub struct FreezeGuard { uid, freeze_amount, settled: AtomicBool, /* Drop impl */ }
```

**`gw-proxy` — 编排层**：

```rust
pub struct ReqCtx {
    pub trace_id:           TraceId,
    pub request_id:         String,
    pub client_codec:       AnyCodec,                 // ADR-019
    pub upstream_codec:     AnyCodec,
    pub user:               Option<Arc<Versioned<UserEntry>>>,
    pub merged_perm:        Option<MergedPermissions>,
    pub model:              Option<ModelName>,
    pub is_stream:          bool,
    pub idempotent:         bool,                     // ADR-022 幂等保护
    pub cached_body:        Option<bytes::Bytes>,
    pub picked_vendor_key:  Option<Arc<VendorKey>>,
    pub picked_vendor:      Option<Arc<Versioned<Vendor>>>,
    pub pricing:            Option<Arc<Versioned<Pricing>>>,
    pub timing:             ReqTiming,
    pub usage:              UsageAccumulator,
    pub freeze_guard:       Option<FreezeGuard>,       // ADR-004 + ADR-015
    pub stream_slot:        Option<StreamSlot>,        // ADR-024 并发限制 RAII
    pub bytes_sent_to_client: AtomicU64,               // 流式重试判定（ADR-022）
}
```

### 4.4 加载链（统一）

任意 entity 资源 miss 时统一走 ADR-016 + ADR-017：

```rust
Moka.get_with(id, async {
    // L1: Redis Entity 协议
    if let Some(raw) = fred.hgetall(format!("entity:{ns}:{id}")).await? {
        let v_redis = raw.parse_v()?;
        let value = if ns == "vendor_key" {
            // ADR-017: 解密 ciphertext + nonce + auth_tag → SecretString
            decrypt_vendor_key(&raw, &kek_cache)?
        } else {
            decode_payload(&raw)?
        };
        return Ok(Versioned { version: v_redis, value: Arc::new(value) });
    }

    // L2: PgSQL 直连 (sqlx)
    let row = pg_loader::<Ns>::fetch(&pg_pool, id).await?
        .ok_or(LoadError::NotFound)?;
    let value = if ns == "vendor_key" {
        let encrypted: EncryptedKeyEntry = serde_json::from_value(row.payload)?;
        decrypt_vendor_key_from_encrypted(&encrypted, &kek_cache)?  // 内存中解密
    } else {
        serde_json::from_value(row.payload)?
    };
    let v_new = row.version as u64;

    // L3: 写回 Redis（带 TTL，受 upsert_if_newer 保护）
    let bytes = if ns == "vendor_key" {
        // 写回时仍是 EncryptedKeyEntry 形式（永不写明文 api_key 到 Redis）
        serde_json::to_vec(&encrypted)?
    } else {
        serde_json::to_vec(&value)?
    };
    let pipe = fred.pipeline();
    pipe.hset(&key, &[("v", v_new.to_string()), ("enc", "json"), ("d", bytes_b64)]);
    pipe.expire(&key, ttl_for_namespace(Ns));
    let _ = pipe.flush().await;     // 写失败仅 metrics，不阻塞响应

    Ok(Versioned { version: v_new, value: Arc::new(value) })
})
```

任何步骤失败 → 当前请求 503 `GW_BACKEND_UNAVAILABLE`，缓存不污染。Rust 不做 negative caching。

### 4.5 计费写入（Rust → Redis）

```
# 单 batch flush（PIPELINE，ADR-004 + ADR-026）
INCRBY counter:user:<uid>:balance_micro <-actual_cost>
INCRBY counter:user:<uid>:tokens_total  <total_tokens>
INCRBY counter:user:<uid>:tokens_today_<YYYYMMDD> <total_tokens>
EXPIRE counter:user:<uid>:tokens_today_<YYYYMMDD> 129600       # 36h
INCRBY counter:vendor_key:<kid>:used_count 1                    # FRD §9.6

XADD billing:records * <SimpleLogRecord 所有字段>               # ADR-026 schema

# 余额跨 0 时（每个用户每次断路只发一次）
XADD alerts * type=balance_zero severity=warn ns=user id=<uid> data='{"ts_ms":...}'
```

注意：
- Rust **不**写 entity hash 字段（user 元数据、Vendor Key 配置等）；写入由管理面或迁移脚本完成。
- Rust 写 entity 仅限 cold-path 回写（带 TTL，受 versioned upsert 保护，ADR-016）+ `entity:ipblock` 自动封禁写入（ADR-025）。
- `counter:vendor_key:<kid>:used_count` 达 `max_usage_count` → 网关将该 Key 状态切到 `depleted`（ADR-021）。

---

## 5. 请求生命周期

### 5.1 完整时序

```
┌────────┐  ┌────────────┐  ┌──────────────────┐  ┌──────────┐  ┌──────────┐
│ Client │  │ Pingora    │  │ EntityStore +    │  │ Upstream │  │ Ledger   │
│        │  │ Filters    │  │ Counters / Auth  │  │ (HTTPS)  │  │ Worker   │
└───┬────┘  └─────┬──────┘  └────────┬─────────┘  └─────┬────┘  └─────┬────┘
    │ POST /v1/chat │                │                  │             │
    │──────────────►│ early_request_filter:             │             │
    │               │   1. 黑名单 / 白名单 IP 校验 (ADR-025)         │             │
    │               │   2. 全局并发流式上限 (ADR-024)                │             │
    │               │   3. 协议 detect_request → client_codec (ADR-019)│           │
    │               │ request_filter:                   │             │
    │               │   4. JWT 验签 / API Key 校验 (gw-auth)         │             │
    │               │      jwks miss → entity:jwks → sqlx           │             │
    │               │   5. 签名校验 (ADR-025, 可选)                  │             │
    │               │   6. 读 body + client_codec.parse_request → IR │             │
    │               │   7. merge_permissions (gw-auth, ADR-023)      │             │
    │               │   8. 模型权限校验                              │             │
    │               │   9. 多级限流 + 并发流式占位 (ADR-024)         │             │
    │               │   10. 单请求 hard cap (ADR-024)                │             │
    │               │   11. 估算冻结 (ADR-004)                       │             │
    │               │       FreezeGuard 挂入 ReqCtx                  │             │
    │               │   12. USER_CIRCUIT 检查                        │             │
    │               │ request_body_filter:               │             │
    │               │   注入 ctx.cached_body                          │             │
    │               │ upstream_peer:                     │             │
    │               │   13. model_group + pricing 懒加载              │             │
    │               │   14. get_candidate_keys (ADR-021)              │             │
    │               │   15. Strategy::pick (ADR-003)                 │             │
    │               │   16. picked_key 解密后即 SecretString          │             │
    │               │   17. Vendor.base_urls 主备选 active            │             │
    │               │   18. Timeouts::apply_to_peer (ADR-009)        │             │
    │               │ upstream_request_filter:           │             │
    │               │   19. upstream_codec.render_request → buf      │             │
    │               │   20. 注入 Authorization (per Vendor.auth_template)│         │
    │               │   21. transparent traceparent 透传              │             │
    │               │   ─────────────────────────────►  │             │
    │◄──────────────┼─ stream chunks ─────────────────┤              │
    │               │ response_body_filter:              │             │
    │               │   22. parse_stream_chunk → StreamEventIR        │             │
    │               │   23. (跨协议) render_stream_chunk → client buf │             │
    │               │   24. 累计 usage、idle timer 重置                │             │
    │               │   25. stream_total deadline 检查                 │             │
    │               │ logging:                          │             │
    │               │   26. FreezeGuard.settle(actual)                │             │
    │               │   27. BREAKERS.record(success/fail)             │             │
    │               │   28. RateLimitTicket.drop → 记录消耗            │             │
    │               │   29. SimpleLogRecord 构造 + 脱敏                │             │
    │               │   30. ledger.mpsc.try_send ───────┼─────────────►│             │
    │               │   31. DetailedLog 异步写对象存储                 │             │
    │               │ ReqCtx::drop → FreezeGuard / StreamSlot RAII   │             │
                                                                       │
                                                       Worker:          │
                                                         100 条 / 200ms  │
                                                         PIPELINE:       │
                                                           INCRBY×N     ─┼─►Redis
                                                           XADD billing  ─┼─►Redis
```

### 5.2 关键 Phase 行为

| Phase | 行为 | 阻塞点 | 错误处理 |
|---|---|---|---|
| `early_request_filter` | TraceId 注入、IP 黑名单、全局并发流式校验、协议 detect | 无（内存） | 403 / 429 / 400 `GW_REQUEST_PROTOCOL_UNKNOWN` |
| `request_filter` | JWT/APIKey、body 读、IR parse、merge_permissions、限流、hard cap、估算冻结、ctx 准备 | 任一 entity miss → Redis(≤1ms) → PgSQL(≤50ms) | 401 / 402 / 403 / 404 / 413 / 429 / 503 |
| `request_body_filter` | 注入 cached_body | 无 | 不应失败 |
| `upstream_peer` | model_group / pricing 懒加载、Strategy.pick、Vendor 主备、超时设置 | 仅冷 group 时 miss | 503 `GW_ROUTING_NO_AVAILABLE_CHANNEL` |
| `upstream_request_filter` | upstream_codec.render_request、Authorization 注入、traceparent 透传 | 无 | 不应失败（构造错误 → `GW_GATEWAY_INTERNAL`） |
| `connected_to_upstream` | in_flight 增加、连接 metric | 无 | pingora 捕获 |
| `response_body_filter` | parse_stream_chunk、render_stream_chunk、累 usage、idle/total deadline | 无（增量） | 流式 timeout 中途 → 发 error event 关闭流 |
| `logging` | FreezeGuard.settle、BREAKERS.record、RateLimit consume、simple_log / detailed_log 构造 | 无（mpsc try_send） | mpsc 满 → 计量丢失 + alert |
| `ReqCtx::drop` | FreezeGuard / StreamSlot RAII 兜底解冻 / 释放槽位 | 无 | metrics: freeze_leak_recovered |

### 5.3 错误响应映射

| 场景 | HTTP | error code | retryable |
|---|---|---|---|
| IP 黑名单命中 | 403 | `GW_AUTH_IP_BLOCKED` | ❌ |
| IP 白名单不通过 | 403 | `GW_AUTH_IP_NOT_ALLOWED` | ❌ |
| JWT 缺失/无效/过期 | 401 | `GW_AUTH_INVALID_JWT` | ❌ |
| API Key 无效 | 401 | `GW_AUTH_INVALID_APIKEY` | ❌ |
| 签名校验失败 | 401 | `GW_AUTH_SIGNATURE_INVALID` | ❌ |
| API Key 无模型/端点权限 | 403 | `GW_AUTH_NO_KEY_PERMISSION` | ❌ |
| 协议识别失败 | 400 | `GW_REQUEST_PROTOCOL_UNKNOWN` | ❌ |
| 输入 token 超 hard cap | 413 | `GW_REQUEST_INPUT_TOO_LARGE` | ❌ |
| 预估成本超限 | 413 | `GW_REQUEST_COST_TOO_HIGH` | ❌ |
| 内容审计拦截（v1 默认不启用）| 400 | `GW_REQUEST_CONTENT_REJECTED` | ❌ |
| 模型不存在 | 404 | `GW_REQUEST_MODEL_NOT_FOUND` | ❌ |
| RPM 触限 | 429 | `GW_QUOTA_RPM_EXCEEDED` | ✓（带 Retry-After）|
| TPM 触限 | 429 | `GW_QUOTA_TPM_EXCEEDED` | ✓ |
| 并发流式超限 | 429 | `GW_QUOTA_CONCURRENT_EXCEEDED` | ✓ |
| 余额不足（估算冻结失败）| 402 | `GW_QUOTA_BALANCE_ZERO` | ❌ |
| 候选 Vendor Key 全开熔断 | 503 | `GW_ROUTING_NO_AVAILABLE_CHANNEL` | ❌ |
| 上游 connect/read/total timeout | 504 | `GW_UPSTREAM_TIMEOUT` | ✓（非流式 / 流式未首字节）|
| 上游 5xx + 重试用尽 | 502 | `GW_UPSTREAM_5XX` | — |
| 上游过载 | 503 | `GW_UPSTREAM_OVERLOADED` | — |
| 上游 4xx | 透传 | `GW_UPSTREAM_4XX` | ❌（不重试、不计费）|
| Redis+PgSQL 同时不可达 | 503 | `GW_BACKEND_UNAVAILABLE` | ❌ |
| 客户端断开 | 499 | `GW_UPSTREAM_CANCELLED` | ❌ |
| 网关 panic / 内部错误 | 500 | `GW_GATEWAY_INTERNAL` | ❌ |

---

## 6. 模块结构与库选型

### 6.1 Cargo Workspace（按 ADR-018，9 crate）

```
llm-gateway/
├── Cargo.toml                  # workspace; release profile lto = "thin"
├── crates/
│   ├── gw-core/                # 纯类型层：IR / ErrorIR / Versioned / ID newtype / KeyStatus / trait 扩展点 / Mutations
│   ├── gw-codec-macros/        # proc-macro = true：IrConvert / IrStreamConvert / IrErrorConvert / DetectRequest 派生宏
│   ├── gw-codec/               # ProtocolCodec trait + openai/anthropic/gemini 三 mod（含 wire-struct + #[derive] + via 函数）+ AnyCodec enum + passthrough fast path
│   ├── gw-store/               # fred(Redis) + sqlx(PgSQL) + EntityStore (Moka) + CounterStore + DeductionState + StreamConsumer
│   ├── gw-auth/                # JWT + APIKey + merge_permissions + IP 黑白名单 + 签名 + 限流 + 单请求 hard cap
│   ├── gw-routing/             # Strategy trait + 8 实现 + BreakerStore + RetryExecutor + Fallback
│   ├── gw-ledger/              # mpsc ingest + flush worker + BillingRecord + FreezeGuard
│   ├── gw-proxy/               # ProxyHttp impl + 所有 filter + ReqCtx + 详细日志 writer
│   └── gw-gateway/             # bin：main + figment config + tracing/metrics init + KMS init + Pingora server
└── docs/superpowers/specs/...
```

依赖 DAG（无环，ADR-018）：

```
gw-gateway → gw-proxy → { gw-codec, gw-auth, gw-routing, gw-ledger } → gw-store → gw-core
                              │                                                ↗
                              ▼
                       gw-codec-macros (proc-macro)
```

`gw-codec-macros` 是 proc-macro crate（语言强约束需独立成 crate）；仅被 `gw-codec` 依赖；它本身不依赖 `gw-core`，生成的代码通过完全限定路径 `::gw_core::*` 引用类型。

### 6.2 核心 crate 选型

| 槽位 | crate | features / 备注 |
|---|---|---|
| HTTP 代理 | `pingora` 0.8 + `pingora-proxy` 0.8 | `openssl` 或 `rustls` 二选一 |
| **proc-macro 基础** | `proc-macro2` + `syn` + `quote` | `gw-codec-macros` 内部依赖；生成 derive 代码 |
| Redis | `fred` | `dynamic-pool, partial-tracing, metrics, subscriber-client` |
| PgSQL | `sqlx` | `postgres, runtime-tokio-rustls, macros, migrate`；编译时 SQL 校验 |
| JSON 热路径 | `sonic-rs` | `get_from_str` 抽单字段，SIMD |
| JSON 非热路径 | `serde` + `serde_json` | 配置 / entity payload |
| JWT | `jsonwebtoken` | 验签结果用 `moka` 短 TTL 缓存 |
| Entity / Counter cache | `moka` (future) | TTL + max_capacity + single-flight |
| 高频写小状态 | `dashmap` + `std::sync::atomic` | 熔断器 / DEDUCT / 限流计数 |
| 限流 | `governor` | GCRA / 多维度令牌桶 |
| 配置 | `figment` | toml + env，支持 reload signal |
| 日志 | `tracing` + `tracing-subscriber` + `tracing-appender` | JSON 输出 |
| Metrics | `metrics` + `metrics-exporter-prometheus` | 独立 9090 端口 |
| OTLP | `opentelemetry` + `opentelemetry-otlp` | 可选 |
| 错误 | `thiserror` + `anyhow` | 库 vs 应用 |
| 异步 | `tokio` 1.x | full |
| 字节 | `bytes` + `bytestring` | 与 pingora 兼容 |
| 秘密 | `secrecy` | `SecretString` for api_key / JWT 私钥 |
| 加密 | `aes-gcm` + `aws-sdk-kms` 或 `vaultrs` | ADR-017 |
| URL | `url` | base_url 解析 |
| 时间 | `time` 0.3 | 单一时间库 |
| HMAC（签名）| `hmac` + `sha2` + `subtle` | ADR-025；`subtle::ConstantTimeEq` |
| 对象存储 | `aws-sdk-s3` 或 `opendal` | detailed_log + debug-trace 写出 |
| **全局 allocator + 内存采样** | `tikv-jemallocator` + `jemalloc_ctl` | ADR-027 debug 模式按 phase 读 `thread.allocated`；release 默认开启（heap 性能更好且免费提供 stats）|
| 后台健康探活 | `reqwest` (rustls) | 仅探活号池上游用；不进 cold path |

> **已废弃**：`arc-swap` / `im`（统一 Moka）；`admin-client`（cold path 改 sqlx 直连）；`reqwest` 在热路径（pingora 内置 H2）。

### 6.3 关键 trait 草稿（汇总自 ADR-019/020/003/022/025/026）

```rust
// gw-core
pub trait ProtocolCodec { /* parse/render 8 方法 + estimate_input_tokens */ }
pub trait Entity { const NAMESPACE: &'static str; type Id; type Value; fn decode(...) -> ...; }
pub trait ContentFilter { /* check_request / check_stream_chunk / check_response */ }
pub trait TokenEstimator { fn estimate(&self, text: &str, model: &str) -> u32; }
pub trait Redactor { fn redact(&self, s: &mut String); }

// gw-routing
pub trait Strategy {
    fn name(&self) -> &'static str;
    fn pick<'a>(&self, candidates: &'a [Arc<VendorKey>],
                breakers: &BreakerStore, ctx: &PickCtx) -> Option<&'a Arc<VendorKey>>;
}

// gw-ledger
pub trait Ledger {
    fn try_send(&self, rec: BillingRecord) -> Result<(), LedgerError>;
    fn freeze(&self, uid: &UserId, amount: u64) -> FreezeGuard;
    fn refresh_base(&self, uid: &UserId, base_micro: i64);
}
```

---

## 7. 性能预算与容量规划

### 7.1 热路径目标延迟（不含上游 RTT）

| Phase | P50 | P99 | 备注 |
|---|---|---|---|
| `early_request_filter`（IP / 并发计数）| < 5µs | < 20µs | DashMap atomic |
| 协议 detect_request | < 20µs | < 50µs | 路径前缀 + 头匹配 |
| JWT 验签（jwks 命中）| < 50µs | < 200µs | FRD AC-5.1-1 |
| `parse_request`（同协议透传，8KB）| < 100µs | < 400µs | sonic-rs get_from_str + 借用 |
| `merge_permissions`（10 个分组）| < 5µs | < 5µs | 内存合并 |
| 限流校验 | < 10µs | < 30µs | governor 本地令牌桶 |
| 估算冻结 | < 1µs | < 5µs | atomic CAS |
| `request_filter` 总（全命中）| < 200µs | < 1ms | 各项累加 |
| `request_filter`（任一 miss → Redis）| < 1.5ms | < 3ms | Redis HGETALL |
| `request_filter`（miss → PgSQL）| < 30ms | ≤ 60ms | 含解密 + 回写 |
| `upstream_peer`（model_group 已缓存）| < 50µs | < 200µs | candidates + pick |
| `render_request`（同协议透传）| < 50µs | < 200µs | 字节直转 |
| `render_request`（跨协议）| < 500µs | < 2ms | 完整 serialize |
| `parse_stream_chunk` × N（流式）| < 5µs/chunk | < 20µs/chunk | sonic-rs |
| `logging` | < 50µs | < 200µs | mpsc try_send |

冷启动后第一次访问每种 entity 都经历一次 miss 路径，之后稳态进入"全命中"区间。

### 7.2 协议层 SLA（对齐 FRD AC-2.3）

| 场景 | P50 | P99 |
|---|---|---|
| OpenAI → OpenAI 同协议透传（无 mutation，8KB） | ≤ 200µs | ≤ 800µs |
| OpenAI → Anthropic 跨协议（8KB） | ≤ 800µs | ≤ 2ms |
| 流式 TTFB 同协议（网关引入开销） | ≤ 5ms | ≤ 20ms |
| 流式跨协议逐 chunk 延迟 | — | ≤ 1ms |
| 流式 idle 触发误差 | — | ≤ 500ms |

### 7.3 容量公式

#### 计费 mpsc channel

```
channel_capacity = peak_qps × max_flush_interval_sec × safety_factor
                 = 5000 × 0.2 × 3 = 3000 → 取 65_536（FRD §15.2.5 默认）
```

flush 间隔 `max(100 records, 200ms)`。channel 满 → 丢弃 + `XADD alerts` + 后续请求 503 `GW_BACKEND_UNAVAILABLE`。

#### Entity caches（每 namespace 独立 Moka）

| Cache | max_capacity | TTL | 估算字节 / entry | 上限内存 |
|---|---|---|---|---|
| `user` | 1,000,000 | 5min | ~ 384B（加 parent_id / group_ids / markup）| ~ 384MB |
| `user_group` | 10,000 | 5min | ~ 256B | ~ 2.5MB |
| `apikey` | 100,000 | 5min | ~ 256B | ~ 25MB |
| `vendor` | 100 | 30min | ~ 1KB | ~ 100KB |
| `vendor_key` | 10,000 | 30min | ~ 512B（含 SecretString）| ~ 5MB |
| `model_group` | 1,000 | 30min | ~ 8KB | ~ 8MB |
| `pricing` | 1,000 | 30min | ~ 384B | ~ 384KB |
| `jwks` | 100 | 1h | ~ 1KB | ~ 100KB |
| `ipblock` | 10,000 | 5min | ~ 128B | ~ 1.3MB |

合计稳态内存 ~ 425MB；按 LRU 淘汰自动收敛。

### 7.4 PgSQL 连接池

- 每 pod `sqlx::PgPool` max=16、min=2、acquire_timeout=200ms、idle_timeout=10min
- pod 数 × 16 应小于 `max_connections`；50 pod × 16 = 800 预留
- statement_timeout 50ms（cold-path 单条主键查询）
- pod 数 > 50 → PgBouncer transaction-mode（参见 ADR-016 重评信号）

### 7.5 Redis 流量预估

- **读**：稳态 XREADGROUP（BLOCK 1s）+ 极少的 cold-path HGETALL；冷启动突发数十并发。
- **写**：计费 PIPELINE 每秒 ~ 10 批 × 100 INCRBY + 100 XADD；限流 counter 同步每 60s 一波。
- 5000 QPS、1000 活跃用户场景下 Redis CPU < 30%（单核），KeyDB/Dragonfly 多核版可线性扩展。

---

## 8. PgSQL 与 Go 站契约

> 本节是 Rust 与 PgSQL / Go 之间的硬约束清单。Go 当前未上线，按 ADR-017 / §4 schema 设计；Go 上线后接管 entity:* 永久写入。

### 8.1 当前阶段 vs Go 上线后

| Key / 表 | 当前阶段 Writer | Go 上线后 Writer | Reader |
|---|---|---|---|
| PgSQL 表（10+ 张）| 迁移脚本 / 人工 SQL | Go 事务（`UPDATE ... SET version = version + 1`）| Rust (sqlx, cold path) |
| `entity:<ns>:<id>`（带 TTL）| Rust cold-path 回写 | Rust 短期影子（带 TTL，受 ADR-013 协调）| Rust |
| `entity:<ns>:<id>`（永久）| 不使用 | Go（HSET，无 EXPIRE）| Rust |
| `entity:ipblock:<id>` | Rust 自动封禁写入（带 expires_at TTL）| Rust + Go 双 writer（按规则协调）| Rust |
| `events:entity` | 不使用 | Go XADD | Rust XREADGROUP |
| `counter:user:<uid>:balance_micro` | Rust 扣账 INCRBY <-cost> | Go 充值 / Rust 扣账 双 writer | Rust + Go |
| `counter:user:<uid>:tokens_*` | Rust INCRBY | 同上 | Rust + Go（聚合）|
| `counter:vendor_key:<id>:used_count` | Rust INCRBY 1 | 同上 | Rust + Go |
| `counter:ratelimit:*` | Rust 周期 INCRBY | 同上 | Rust |
| `events:counter` | 不使用 | Go XADD（充值 / 退款）| Rust XREADGROUP |
| `billing:records` | Rust XADD | 同上 | Go XREAD |
| `alerts` | Rust XADD | 同上 | Go XREAD |

### 8.2 PgSQL 必读列契约（最小集，ADR-021 + ADR-023 + ADR-017）

#### 公共字段（每张表必有）

```sql
id           <PK>          NOT NULL,
version      BIGINT        NOT NULL DEFAULT 0,    -- ADR-006 版本仲裁
updated_at   TIMESTAMPTZ   NOT NULL DEFAULT now(),
deleted_at   TIMESTAMPTZ   NULL                   -- 软删除；Rust 仅读 deleted_at IS NULL
```

#### `users`（namespace=`user`）

```sql
user_id                  TEXT        PK,
parent_id                TEXT        NULL  REFERENCES users(user_id),  -- NULL=主账户
tier                     TEXT        NOT NULL,
status                   TEXT        NOT NULL,                          -- "active" | "banned"
allowed_models           TEXT[]      NOT NULL DEFAULT '{}',
allowed_channels         TEXT[]      NOT NULL DEFAULT '{}',
allowed_groups           TEXT[]      NOT NULL DEFAULT '{}',
rpm_limit                INTEGER     NOT NULL DEFAULT 0,
tpm_limit                INTEGER     NOT NULL DEFAULT 0,
max_concurrent_streams   INTEGER     NOT NULL DEFAULT 100,
daily_quota_tokens       BIGINT      NOT NULL DEFAULT 0,
user_group_ids           TEXT[]      NOT NULL DEFAULT '{}',
markup                   REAL        NOT NULL DEFAULT 1.0
-- + 公共字段
```

#### `user_groups`（namespace=`user_group`）

```sql
group_id              TEXT        PK,
display_name          TEXT        NOT NULL,
priority              INTEGER     NOT NULL DEFAULT 0,
allowed_models        TEXT[]      NOT NULL DEFAULT '{}',
allowed_channels      TEXT[]      NOT NULL DEFAULT '{}',
model_group_binding   TEXT        NULL,
rpm_limit             INTEGER     NOT NULL DEFAULT 0,
tpm_limit             INTEGER     NOT NULL DEFAULT 0,
max_concurrent_streams INTEGER    NOT NULL DEFAULT 0,
markup                REAL        NOT NULL DEFAULT 1.0
-- + 公共字段
```

#### `api_keys`（namespace=`apikey`）

```sql
key_id           TEXT        PK,
user_id          TEXT        NOT NULL REFERENCES users(user_id),
status           TEXT        NOT NULL,           -- "active" | "disabled"
hash             BYTEA       NOT NULL,            -- bcrypt / argon2
allowed_models   TEXT[]      NOT NULL DEFAULT '{}',
allowed_channels TEXT[]      NOT NULL DEFAULT '{}',
rpm_limit        INTEGER     NOT NULL DEFAULT 0,
tpm_limit        INTEGER     NOT NULL DEFAULT 0,
markup           REAL        NOT NULL DEFAULT 1.0,
ip_allowlist     TEXT[]      NULL,                -- CIDR 列表
expires_at       TIMESTAMPTZ NULL
-- + 公共字段
```

#### `vendors`（namespace=`vendor`）

```sql
vendor_id            TEXT        PK,
display_name         TEXT        NOT NULL,
base_urls            TEXT[]      NOT NULL,        -- 主备
auth_template        TEXT        NOT NULL,
connect_timeout_ms   INTEGER     NULL,            -- 覆盖全局
read_timeout_ms      INTEGER     NULL,
total_timeout_ms     INTEGER     NULL,
stream_total_timeout_ms INTEGER  NULL,
tls_config_json      JSONB       NULL,
default_proxy        TEXT        NULL,
contact              TEXT        NULL
-- + 公共字段
```

#### `vendor_keys`（namespace=`vendor_key`，ADR-017 加密存储）

```sql
key_id                  TEXT        PK,
vendor_id               TEXT        NOT NULL REFERENCES vendors(vendor_id),
ciphertext              BYTEA       NOT NULL,    -- AES-256-GCM
nonce                   BYTEA       NOT NULL,    -- 12B
auth_tag                BYTEA       NOT NULL,    -- 16B
kek_kid                 TEXT        NOT NULL,    -- 用于解密的 KEK 标识
accessible_models       TEXT[]      NOT NULL,
model_mapping_json      JSONB       NOT NULL DEFAULT '{}',
accessible_endpoints    TEXT[]      NOT NULL,    -- ["openai","anthropic","gemini"]
protocol_conversion     BOOLEAN     NOT NULL DEFAULT false,
priority                INTEGER     NOT NULL DEFAULT 0,
weight                  INTEGER     NOT NULL DEFAULT 1,
rpm_limit               INTEGER     NOT NULL DEFAULT 0,
tpm_limit               INTEGER     NOT NULL DEFAULT 0,
max_usage_count         BIGINT      NULL,
proxy                   TEXT        NULL,
ip_allowlist            TEXT[]      NULL,
expires_at              TIMESTAMPTZ NULL,
status                  TEXT        NOT NULL,    -- 7 状态机
tags                    TEXT[]      NOT NULL DEFAULT '{}'
-- + 公共字段
```

#### `model_groups`（namespace=`model_group`）

```sql
group_name              TEXT        PK,
strategy                TEXT        NOT NULL,    -- 8 策略之一
cross_vendor_fallback   BOOLEAN     NOT NULL DEFAULT false,
max_tokens_cap          INTEGER     NULL,
default_params_json     JSONB       NOT NULL DEFAULT '{}',
forced_params_json      JSONB       NOT NULL DEFAULT '{}',
vendor_key_ids          TEXT[]      NOT NULL DEFAULT '{}'
-- + 公共字段
```

#### `pricings`（namespace=`pricing`）

```sql
model_name                   TEXT        PK,
input_price_micro_per_token  BIGINT      NOT NULL,
output_price_micro_per_token BIGINT      NOT NULL,
max_output                   INTEGER     NOT NULL DEFAULT 4096,
model_markup                 REAL        NOT NULL DEFAULT 1.0,
connect_timeout_ms           INTEGER     NULL,        -- 覆盖 Vendor / 全局
read_timeout_ms              INTEGER     NULL,
total_timeout_ms             INTEGER     NULL,
stream_total_timeout_ms      INTEGER     NULL
-- + 公共字段
```

#### `jwks` + `ip_blocks` 略（结构简单，详见 §4.1）。

### 8.3 Entity / Counter / Stream 写入要求

详见 ADR-013 Writer 协调规则与 ADR-006 versioned upsert。

### 8.4 KEK 轮换

详见 ADR-017。轮换期间网关同时持有 `kek_kid → key` 映射，按 `vendor_keys.kek_kid` 字段选择 KEK。

### 8.5 schema 演进规则

- 新增 Rust 必读列 → `ALTER TABLE ADD COLUMN ... DEFAULT ...` + backfill → 再发版 Rust
- 改名 / 改类型 → 先发版 Rust 双读 → 迁移 schema → 清理 Rust 旧路径
- 删除：仅软删除（`deleted_at`），**严禁**物理 DELETE（缓存会保留到 TTL 才感知）

---

## 9. 可观测性

### 9.1 Metrics（Prometheus，:9090）

```
# 通用
gateway_requests_total{user, model, group, status, error_code}                Counter
gateway_request_duration_seconds{phase}                                       Histogram
gateway_active_streams{scope=global|user}                                     Gauge

# 协议层（ADR-019）
gateway_codec_parse_duration_seconds{protocol}                                Histogram
gateway_codec_render_duration_seconds{src_protocol, dst_protocol}             Histogram
gateway_codec_passthrough_total{src_protocol, dst_protocol, mode=byte|reserialize} Counter

# 号池（ADR-008/021）
gateway_pool_in_flight{vendor_key_id}                                         Gauge
gateway_pool_breaker_state{vendor_key_id}                                     Gauge   # 0/1/2
gateway_pool_pick_total{strategy, vendor_key_id}                              Counter
gateway_vendor_key_status{vendor_key_id, status}                              Gauge

# 重试（ADR-022）
gateway_retry_attempts_total{vendor_key_id, mode, result}                     Counter
gateway_retry_total_duration_seconds                                          Histogram

# 计费 / 余额（ADR-004/015）
gateway_billing_channel_len                                                   Gauge
gateway_billing_channel_full_total                                            Counter
gateway_billing_flush_duration_seconds                                        Histogram
gateway_billing_flush_failures_total{reason}                                  Counter
gateway_freeze_amount_micro{user}                                             Gauge
gateway_freeze_leak_recovered_total                                           Counter
gateway_user_circuit_open_total{reason}                                       Counter

# 限流（ADR-024）
gateway_ratelimit_rejected_total{scope, dimension=rpm|tpm|concurrent}         Counter
gateway_ratelimit_remaining{scope, id, dimension}                             Gauge

# 安全（ADR-025）
gateway_security_ip_blocked_total{reason}                                     Counter
gateway_security_signature_failed_total                                       Counter
gateway_security_auto_block_total                                             Counter

# 缓存 / 加载链
gateway_entity_cache_size{ns}                                                 Gauge
gateway_entity_cache_miss_total{ns, source=redis|pgsql}                       Counter
gateway_entity_load_duration_seconds{ns, source}                              Histogram

# 加密 (ADR-017)
gateway_kms_decrypt_total{result}                                             Counter
gateway_vendor_key_decrypt_failures_total                                     Counter

# PgSQL (cold path)
gateway_pgsql_pool_size                                                       Gauge
gateway_pgsql_pool_idle                                                       Gauge
gateway_pgsql_query_duration_seconds{ns}                                      Histogram
gateway_pgsql_errors_total{ns, kind}                                          Counter

# 同步
gateway_stream_lag_seconds{stream}                                            Gauge
gateway_stream_apply_total{stream, ns?, scope?, op, result}                   Counter

# 上游
gateway_upstream_request_duration_seconds{provider, model}                    Histogram
gateway_upstream_ttft_seconds{provider, model}                                Histogram
gateway_upstream_5xx_total{provider, model}                                   Counter
gateway_upstream_token_in_total{model}                                        Counter
gateway_upstream_token_out_total{model}                                       Counter

# 日志（ADR-026）
gateway_detailed_log_write_failures_total                                     Counter
gateway_detailed_log_buffer_size                                              Gauge

# Debug 模式（ADR-027）
gateway_debug_traces_active                                                   Gauge
gateway_debug_traces_started_total{trigger}                                   Counter
gateway_debug_traces_rejected_total{reason}                                   Counter
gateway_debug_trace_write_failures_total                                      Counter
gateway_debug_phase_duration_seconds{phase}                                   Histogram
gateway_debug_phase_heap_delta_bytes{phase}                                   Histogram
```

### 9.2 Tracing

每请求生成 `trace_id`（W3C traceparent），通过 `tracing` crate 串联：

```
HTTP listener
└─ early_request_filter
└─ request_filter
   ├─ auth.verify
   ├─ permission.merge
   ├─ ratelimit.check
   ├─ entity.load{ns} (cold path tagged with source=redis|pgsql)
   ├─ codec.parse_request{protocol}
   └─ ledger.freeze
└─ upstream_peer
   ├─ entity.load{ns=model_group}
   ├─ strategy.pick{strategy}
   └─ codec.render_request{src,dst}
└─ upstream_request_filter
└─ response_body_filter
   └─ codec.parse_stream_chunk (sampled)
└─ logging
   ├─ ledger.settle
   ├─ ratelimit.consume
   └─ log.write
```

`traceparent` 透传到上游（FRD §12.5）。OTLP 导出可选；默认仅启用 stdout JSON tracing。

### 9.3 日志

- 错误 / 警告 → JSON line stderr（tracing）
- `simple_log` → `billing:records` Stream（保留 365 天，ADR-026）
- `detailed_log` → 对象存储（保留 7 天，按 1h 分文件 gzip）
- 所有 api_key / JWT / PII 字段 `[REDACTED]`（`secrecy::SecretString` + `Redactor`）

---

## 10. 运维与故障处理

### 10.1 端口分离（FRD §15.2.7）

| 端口 | 默认 | 用途 | 暴露策略 |
|---|---|---|---|
| `server.business_port` | 8000 | HTTP/HTTPS + SSE 业务流量 | 公网（经 LB / WAF）|
| `server.metrics_port` | 9090 | Prometheus `/metrics` | 仅内网（K8s ServiceMonitor）|
| `server.health_port` | 8080 | `/livez` + `/readyz` | 仅内网（kubelet）|

- 三端口在不同 Pingora `Server::add_listener` 中独立监听；
- 健康探针实现见 §10.4，可配 IP 白名单限制为 K8s 节点 CIDR。

### 10.2 启动顺序

```
1. 加载 config（figment：toml + env）
2. 初始化 tracing / metrics（监听 :9090）
3. KMS 初始化：拉 KEK 到内存（ADR-017）；失败 fail-fast
4. 初始化 fred Redis pool + sqlx PgPool（PING / SELECT 1 双探活）
5. 取 X_LAST_entity / X_LAST_counter 锚点（XINFO STREAM）
6. 启动后台 task：
   - Stream consumer × 2 (events:entity / events:counter)
   - Billing flusher
   - DetailedLog writer
   - Health checker（号池 key 探活；按需对已加载 pool_group 的 keys 探活）
   - Key 状态机扫描器（每秒，ADR-021）
   - 限流计数器同步任务（每 60s，ADR-024）
   - 异常流量后台 task（每 30s，ADR-025）
7. 启动健康探针 listener（:8080）
8. 启动业务 listener（:8000）
9. readiness 通过条件：Redis PING < 100ms + PgSQL SELECT 1 < 100ms +
   consumer 已对齐 LAST_ID + 至少 1 个 model_group 有 active Vendor Key

— 不做任何资源预加载（ADR-005）
— 进程启动到 ready 通常 < 200ms（不含 KMS 拉取时间，KMS 一次性 < 1s）
```

### 10.3 关闭顺序（graceful，FRD §13.2）

```
1. K8s preStop hook 调用 → 探针 /readyz 立即返回 503，让 Service 摘流
2. 等待 pre_stop_drain_seconds（默认 10s）让 LB 摘流生效
3. SIGTERM 到来：
   a. 业务 listener stop accept
   b. 已建立连接继续处理（含流式），grace period 默认 60s
   c. 探活 / 健康检查 worker 停止
   d. grace 结束 / stream_total 兜底触发 → 强制关闭剩余流式（发送 error event）
   e. mpsc channel drain → batch flush 余下计费记录到 Redis
   f. DetailedLog writer flush + multipart upload close
   g. Stream consumer XACK 已应用事件 + 关闭
   h. 进程退出
```

### 10.4 健康探针端点（FRD §13.3）

```
GET /livez   → 200 OK 始终（进程存活即可）

GET /readyz  → 200 OK / 503 Service Unavailable
  503 条件（任一）：
    - Redis 主连接最近 30s 内无成功响应
    - PgSQL 连接池无空闲连接（pool_idle == 0 && pool_size 满）
    - 所有 model_group 中无 active Vendor Key
    - 进程处于 shutting_down 状态（preStop 已触发）
```

探针端点不需要鉴权（K8s kubelet 内网调用）；可配置 IP 白名单（如 `127.0.0.1` + K8s 节点 CIDR）。

### 10.5 失败模式与处理

| 失败 | 现象 | 处理（FRD §13.1）|
|---|---|---|
| Redis 全挂 | 缓存命中仍能服务 / 计费 flush 失败 | fail-open：Moka 命中正常服务，cold path miss → 503；mpsc 累积 → 满 → 503；恢复后批 flush |
| Redis 抖动 | 单批 flush 失败 | 重试 3 次（指数退避），仍失败 → metric 报警，**保留 batch 重试** |
| PgSQL 不可达 | Redis 也 miss 时冷加载失败 | 503 `GW_BACKEND_UNAVAILABLE`；热缓存请求不受影响 |
| PgSQL statement timeout | sqlx 报 timeout | 当前请求 503；不影响其他请求 |
| KMS 不可达 | 启动期 → fail-fast；运行期 → 已缓存 KEK 不受影响，无法解密新 vendor_key 时该 Key 不可用 + alert | metrics: `gateway_kms_decrypt_total{result=error}` |
| Stream 严重 lag（> 30s）| 数据陈旧 | metric 报警；超过 5min 触发 readiness 失败让 K8s 重启 |
| 上游 5xx | 单 Key 触发熔断 → Strategy 切换；全 group 熔断 → 503 `GW_ROUTING_NO_AVAILABLE_CHANNEL` | ADR-008 + ADR-022 |
| 全部上游不可达 | 立即返回 502 `GW_UPSTREAM_5XX`，响应体含 `retry_after`（默认 30s）| FRD §13.1 |
| panic | 单请求 → pingora 隔离；后台 task → tokio::spawn 包装重启 + alert | — |
| 冷启动突刺 | 新 pod 上线第一波集中 miss | 接受（ADR-005）；监控 `entity_cache_miss_total{source=pgsql}` 突增可识别 |
| mpsc channel 满 | 计费请求"丢弃 + XADD alert + 本地 metric"，**不阻塞热路径** | FRD §13.1 |
| FreezeGuard leak | logging 未执行 → Drop 自动回滚 frozen + `freeze_leak_recovered_total` 非零 | ADR-004 |

### 10.6 部署拓扑

- 多 AZ 多 pod，每 pod 独立 consumer group / 独立熔断 / 独立限流本地状态
- 业务 8000 公网，metrics 9090 / health 8080 不出公网
- Redis：建议 KeyDB / Dragonfly 多线程版（fred 兼容），单实例可承担数万 QPS
- PgSQL：与 Rust 网关同 VPC，直连；50 pod × 16 = 800 连接预留
- KMS：与 Rust 网关同账号，IAM 限定 `kms:Decrypt` + 特定 key ARN
- Go 站独立部署，与 Rust / PgSQL 同 VPC（上线后接管 entity:* 永久写入）

### 10.7 配置变更（FRD §15）

| 内容 | 变更方式 | 是否需重启 |
|---|---|---|
| 号池整组 / Vendor / VendorKey | 当前阶段：迁移 SQL；Go 上线后：events:entity | 否 |
| 模型分组策略 | 同上（entity:model_group.strategy 字段）| 否 |
| 计价 / 倍率 | 同上（entity:pricing）| 否 |
| 用户元数据 / 分组 / API Key | 同上 | 否 |
| 用户余额 | counter:user:*:balance_micro（Go 上线后 events:counter）| 否 |
| JWKS | events:entity | 否 |
| IP 黑/白名单 | events:entity（ipblock namespace）| 否 |
| PgSQL schema 演进 | §8.5 演进规则 | 视列而定 |
| 全局默认超时 / 限流阈值 / 端口 | 改 `config.toml` 或 env | 是（重启 pod）|
| 新增策略代码 / 新增 Entity namespace / 新增协议 codec | git PR + 重新部署 | 是 |
| KEK 轮换 | KMS 控制台 + 重新加密 + 双 KEK 期 | 否（运行时双密钥）|

---

## 11. 参考资料

### 11.1 框架核心

| 资源 | 链接 |
|---|---|
| Pingora 仓库 | https://github.com/cloudflare/pingora |
| Pingora User Guide | https://github.com/cloudflare/pingora/blob/main/docs/user_guide/intro.md |
| `ProxyHttp` trait API | https://docs.rs/pingora-proxy/0.8.0/pingora_proxy/trait.ProxyHttp.html |
| `Session` API | https://docs.rs/pingora-proxy/0.8.0/pingora_proxy/struct.Session.html |
| `HttpPeer` API | https://docs.rs/pingora-core/0.8.0/pingora_core/upstreams/peer/struct.HttpPeer.html |
| `PeerOptions` | https://docs.rs/pingora-core/0.8.0/pingora_core/upstreams/peer/struct.PeerOptions.html |

### 11.2 关键 Crate

| Crate | 链接 |
|---|---|
| fred | https://docs.rs/fred |
| sqlx | https://docs.rs/sqlx |
| sonic-rs | https://docs.rs/sonic-rs |
| serde / serde_json | https://serde.rs |
| jsonwebtoken | https://docs.rs/jsonwebtoken |
| moka | https://docs.rs/moka |
| dashmap | https://docs.rs/dashmap |
| governor | https://docs.rs/governor |
| figment | https://docs.rs/figment |
| tracing / tracing-subscriber | https://docs.rs/tracing |
| metrics + metrics-exporter-prometheus | https://docs.rs/metrics |
| opentelemetry-otlp | https://docs.rs/opentelemetry-otlp |
| tokio | https://docs.rs/tokio |
| bytes | https://docs.rs/bytes |
| secrecy | https://docs.rs/secrecy |
| aes-gcm | https://docs.rs/aes-gcm |
| aws-sdk-kms / vaultrs | https://docs.rs/aws-sdk-kms / https://docs.rs/vaultrs |
| hmac + sha2 + subtle | https://docs.rs/hmac / https://docs.rs/sha2 / https://docs.rs/subtle |
| aws-sdk-s3 / opendal | https://docs.rs/aws-sdk-s3 / https://docs.rs/opendal |
| anyhow / thiserror | https://docs.rs/anyhow / https://docs.rs/thiserror |

### 11.3 协议与规范

| 资源 | 链接 |
|---|---|
| OpenAI Chat API | https://platform.openai.com/docs/api-reference/chat |
| Anthropic Messages API | https://docs.anthropic.com/en/api/messages |
| Gemini Generate Content | https://ai.google.dev/api/rest/v1beta/models/generateContent |
| OpenAI 流式 usage（include_usage）| https://platform.openai.com/docs/api-reference/chat/streaming |
| SSE 规范（HTML5）| https://html.spec.whatwg.org/multipage/server-sent-events.html |
| Redis Streams | https://redis.io/docs/data-types/streams/ |
| W3C Trace Context | https://www.w3.org/TR/trace-context/ |
| RS256 / ES256 JWT | https://datatracker.ietf.org/doc/html/rfc7518 |
| AES-256-GCM | https://nvlpubs.nist.gov/nistpubs/Legacy/SP/nistspecialpublication800-38d.pdf |

### 11.4 设计参考

| 资源 | 链接 |
|---|---|
| Pingap（基于 Pingora 的网关）| https://github.com/vicanso/pingap |
| LiteLLM（Python 多模型路由参考）| https://github.com/BerriAI/litellm |
| One-API（Go LLM 网关参考）| https://github.com/songquanpeng/one-api |
| 加权轮询算法（SWRR） | https://en.wikipedia.org/wiki/Weighted_round_robin |
| GCRA 限流算法 | https://en.wikipedia.org/wiki/Generic_cell_rate_algorithm |

### 11.5 性能与诊断工具

| 工具 | 用途 |
|---|---|
| `tokio-console` | tokio 任务诊断 |
| `cargo-flamegraph` | CPU profiling |
| `cargo-asm` | 检查 LTO 后 inline 效果（ADR-018 验证）|
| `heaptrack` / `bytehound` | 堆诊断 |
| `vegeta` / `wrk2` / `oha` | HTTP/2 压测，SSE |













