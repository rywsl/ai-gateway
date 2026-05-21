# Gap Residual — 收敛 Inventory 未立项空白桶/残项（v2026-05-21）

> **Integration status**: 与 [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md) 共存。本 spec 处理的是 v2 集成设计未覆盖、且在 [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md) 中**尚未立项成独立 spec** 的能力。已有 spec（A-T0~T6 / C-α~ε / K-A~D / R-AB~G / RL-Core/IP/Adaptive / S1~S5 / Task-Worker）一律不动。

- **日期**：2026-05-21
- **状态**：Draft（待审阅，仅契约与边界，未含实现）
- **作者**：协作设计（Claude + 用户）
- **范围**：按"能力联总"重新聚类 inventory 中以下空白：
  - 桶 7 错误处理（**整桶**：E1–E6）
  - 桶 9 鉴权残项（AU1 / AU2 / AU4 / AU5）
  - 桶 10 能力与路由参数（**整桶**：CP1–CP6 + AU3 / AU7）
  - 桶 1 codec 残项（C6 / C7 / C8 / C9）
  - 桶 12 隧道节点（**整桶**：TN1–TN6）
  - 桶 13 cache invalidation（H2）
  - 桶 14 可观测残项（O1 / O2 / O3 / O5）
  - 桶 15 安全残项（SE3 KEK 轮换）
- **不在范围**：
  - 已立项 spec 的内容（A-T*/C-α-ε/K-A-D/R-AB-G/RL-*/S1-5/Task-Worker）；
  - 计费内核（公式引擎 / 模板 / 历史重计费 / 维度 collector）—— 永久归 Go；
  - provider_ops（SaaS 余额查询 / 签到 / 周期巡检）—— 归 Go；
  - 订阅 / 支付 / license / 邮件 / LDAP / 公告 / 用户管理 —— 归 Go。
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（本 spec 是其"残项收敛"）
  - [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md)（不变量与 crate 分层）
  - [2026-05-19-adapter-a-t0-trait-v2.md](./2026-05-19-adapter-a-t0-trait-v2.md)（§ 1 / § 4 / § 5 多处依赖 adapter trait v2）
  - [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md)（§ 1 错误编排消费 K-A 的 cooldown）
  - [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md) / s2 / s3 / s4 / s5（§ 4 残项依赖 codec 矩阵）
  - [2026-05-19-routing-r-ab-multi-score.md](./2026-05-19-routing-r-ab-multi-score.md) / r-d / r-e（§ 2 capability 决策喂入 routing filter）
  - [2026-05-19-billing-c-alpha-usage-record.md](./2026-05-19-billing-c-alpha-usage-record.md) / c-ε（§ 2.6 / § 6 引用）

## 0. 文档约定

- ADR 编号：`ADR-G<节号>-<序号>`（G = Gap residual），与现有 K-A / R-AB 等同形态。
- 跨 spec 引用以**条目号**（如 `inventory § 桶 7 E2`）+ **文件锚点**双链。
- crate 归属严格遵循 v2 集成设计 § 3 三层规则：
  - 支撑层：`gw-core` `gw-codec(-macros)` `gw-store` `gw-auth` `gw-ledger`
  - 业务层：`gw-adapter` `gw-health` `gw-routing` 以及本 spec 新增的 `gw-capability` `gw-tunnel-client`
  - 执行层：`gw-proxy` `gw-gateway`（**互不依赖** + 不依赖未来 `gw-task-runtime`）
- 不变量 lint：本 spec 引入的新静态约束统一收口在 § 8.2，由 `scripts/check-invariants.sh` 落地。

---

## 1. 错误编排子系统（Error Orchestration）

> **覆盖**：inventory 桶 7 整桶（E1–E6）+ 桶 8 失败转移在错误处理侧的语义补丁（与 R-E / RL-Core 通过事件解耦，不抢 R-E 的执行职责）+ 桶 15 SE3 KEK 轮换路径（运维型错误恢复，归此节）。

### 1.1 背景

当前 `gw-proxy::error_emit` 只做"短路时发一条 vendor 形态的 JSON 错误体"。错误**分类**与**副作用**完全揉在 filter 里，散落在：

- `filters::request` —— 鉴权失败 / RPM 超限 / 余额不足 → `error_emit::send_error` + 直接 return。
- `filters::upstream` —— peer 选不出 → 同上。
- `filters::response` —— 上游 5xx 直接透传字节，无分类。
- `filters::logging` —— freeze release / key-health record 是各自独立调用，没有"按错误码统一编排"。

Aether 的错误编排是 4 件套：
1. `error_classifier`（**纯函数**，无副作用，把 raw error 翻译成 ErrorClass + retryability）；
2. `error_handler`（**多侦听器** fan-out：health record / RPM 调整 / OAuth 标记 / cache 失效 / metrics）；
3. `error_passthrough`（上游真实错误体 → 客户端协议错误体翻译）；
4. provider 特定的 `error_enhancer`（kiro 加帧头、claude 改 type 字段等）。

加上 `resilience` 全局降级 + thinking signature 错触发整流，是请求生命周期中"出错路径"的全部能力。

### 1.2 关键判断

#### ADR-G1-1 ErrorClass 是 `gw-core::error` 的扩展，不新建 crate

理由：
- ErrorClass 是**类型**（无 I/O、无运行时状态），归支撑层。
- 现 `gw-core::error::GwError` 已是错误码 enum，新增 `ErrorClass` + `From<GwError> for ErrorClass` 即可。
- 不为分类器单独建 crate，避免 v2 三层之外多一层。

```rust
// gw-core::error
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorClass {
    /// 鉴权失败（401/403 from 网关本身，非上游）
    AuthDenied,
    /// 余额 / 配额 / 硬上限（网关判定）
    QuotaDenied,
    /// 网关侧 RPM
    RateLimitedSelf,
    /// 上游 RPM / TPM / Daily / Concurrent（K-B 已分类）
    RateLimitedUpstream(RateLimitKind),
    /// 上游账号级阻断（401 账号删除 / 403 账号挂起 / OAuth refresh failed）
    AccountBlocked(AccountBlockReason),
    /// 上游瞬时 5xx / 408 / 529
    UpstreamTransient,
    /// 协议错误（codec parse 失败、不支持的形状）
    ProtocolError,
    /// thinking signature 错触发（参 codec C3）
    ThinkingSignatureMismatch,
    /// 客户端连接异常（断流、超时）
    ClientAbort,
    /// 配置 / 启动 / 内部 bug 类
    Internal,
}

// RateLimitKind 由 RL-Core 拥有（见 2026-05-19-ratelimit-rl-core.md）。
// 本 spec 仅 use；不许重新声明。

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountBlockReason {
    Status401AccountDeactivated,
    Status401Unauthorized,
    Status402PaymentRequired,
    Status403AccountSuspended,
    Status400OrganizationDisabled,
    OAuthRefreshFailed,
    AdminForce,
}
```

`ErrorClass` 与 K-A 的 `CooldownReason` **故意不强耦合**：
- ErrorClass 是"对客户端 / 编排层"的语义；
- CooldownReason 是"对 KeyHealthStore 内部"的状态标记；
- 一份 `ClassifierOutcome` 可同时给出 ErrorClass + CooldownDirective，由 § 1.3 的 classifier 负责映射。

#### ADR-G1-2 Classifier 是 trait + 默认实现

```rust
// gw-core::error::classify
pub trait ErrorClassifier: Send + Sync {
    /// 网关自身判定错误（鉴权 / RPM / 余额）。
    fn classify_local(&self, err: &GwError) -> ClassifierOutcome;

    /// 上游响应错误（HTTP 状态 + 头 + 解析后 vendor 错误体）。
    /// vendor_payload 为 None 表示无法解析（坏 JSON / 非 JSON / 无 body）。
    fn classify_upstream(
        &self,
        ctx: &ClassifyCtx<'_>,
        http_status: u16,
        headers: &dyn HeadersAccessor,
        vendor_payload: Option<&serde_json::Value>,
    ) -> ClassifierOutcome;
}

pub struct ClassifierOutcome {
    pub class: ErrorClass,
    pub retryable: Retryability,
    pub cooldown: Option<CooldownDirective>,   // 喂给 K-A
    pub rate_limit_hint: Option<RateLimitHint>, // 喂给 RL-Adaptive
    pub oauth_signal: Option<OAuthSignal>,      // 喂给 K-C
    pub passthrough_body: Option<PassthroughBody>, // § 1.5 用
}
```

默认实现 `DefaultClassifier` 处理"没有 adapter 介入时的兜底"（HTTP 码硬编码映射）。**vendor 特定**（如 kiro 错误体含 `errorCode`、anthropic 含 `type=overloaded_error`）的优化由 adapter 通过 K-B 已有的 `error_analyzer` trait 注入；本节不重新引一套 trait。

`ClassifyCtx<'_>` 携带 `KeyId / VendorId / ApiFormat / ProtocolKind`，用于 OAuth 信号判定。

#### ADR-G1-3 Handler 是多侦听器 fan-out

```rust
// gw-proxy::error::handler
pub struct ErrorHandler {
    listeners: Vec<Arc<dyn ErrorListener>>,
}

pub trait ErrorListener: Send + Sync {
    /// 在 outcome 已分类、即将发响应前被调用。所有 listener 串行 + 同步执行
    /// （都是内存操作，不许做 I/O / await）。需 I/O 的 listener 自己 spawn。
    fn on_outcome(&self, ctx: &HandlerCtx<'_>, outcome: &ClassifierOutcome);
}
```

v1 的标准 listener 集合（**全部硬编码注册**，不做插件化；插件化只对 § 4 codec / § 6 metrics 这类纯增强场景考虑）：

| Listener | crate | 副作用 |
|---|---|---|
| `KeyHealthListener` | `gw-proxy` | `cooldown` → `KeyHealthStore::apply_cooldown`；ErrorClass=Success/Transient → `record` |
| `RateLimitFeedbackListener` | `gw-proxy` | `rate_limit_hint` → RL-Adaptive 的 `observe(...)` |
| `OAuthListener` | `gw-proxy` | `oauth_signal` → K-C 的 `mark_refresh_failed` / `mark_blocked` |
| `CacheInvalidateListener` | `gw-proxy` | 401 / 403 → 触发 EntityStore 失效（与 § 2.9 配套） |
| `MetricsListener` | `gw-proxy` | `error_class` 维度计数 + status-class collapse 修复（O6） |
| `LedgerListener` | `gw-ledger` | 写 detailed_log + freeze release（与现 `filters::logging` 同语义，迁移过来） |

**listener 注册顺序固定**（KeyHealth → RateLimit → OAuth → CacheInvalidate → Metrics → Ledger）。后续 listener 不能依赖前面 listener 的副作用观察结果——彼此通过 `outcome` + `HandlerCtx` 通信。

#### ADR-G1-4 Passthrough 由 codec 完成，不进 handler

`error_passthrough`（E3）是"上游 vendor 错误体 → 客户端协议错误体"的翻译，本质是 codec 矩阵的一部分。**接口归 `gw-codec`**：

```rust
// gw-codec::error
pub fn render_error_to_client(
    client_protocol: ProtocolKind,
    upstream_payload: &PassthroughBody,
    fallback_class: ErrorClass,
) -> Bytes;
```

`PassthroughBody` 由 classifier 在 `classify_upstream` 时构造，包含：
- 上游原始 JSON 值（已脱敏，去掉 vendor 内部 trace_id 等）；
- 推断出的客户端期望状态码；
- 是否 SSE（流中错误需走 `event: error` 帧）。

**vendor 特定的 error enhancer**（E4 kiro）走 K-B 已有的 `error_analyzer` trait —— 把 `analyze` 的返回值扩成 `AnalyzerOutcome { cooldown, enhanced_body, oauth_signal }`，本节**不**新加 trait；K-B spec 在落地时同步收口此扩展（见 § 7 反向引用）。

#### ADR-G1-5 Resilience 全局降级（E5）

降级范围**仅限"网关自身组件不可用"**：
- Redis 不可达：read 走 EntityStore in-mem cache（已有），write（freeze / cooldown / RL counter）转 in-process fallback；
- Postgres 不可达：rejected at startup（v1 不做运行期重连降级）；
- KMS 不可达：rejected at startup（KEK 在内存，不需要运行期访问 KMS）；
- Ledger flusher 阻塞：drop oldest（已有 `try_send` 失败计数）。

```rust
// gw-proxy::resilience
pub struct ResilienceState {
    pub redis_state: AtomicU8, // Healthy / Degraded / Unavailable
    pub last_redis_error_ms: AtomicU64,
    pub fallback_freeze_used: AtomicU64,
    pub fallback_cooldown_used: AtomicU64,
}
```

**friendly error**（用户可见错误体）由 § 1.4 的 codec passthrough + classifier 给出的 `ErrorClass` → 默认错误模板**单点**生成，避免每个 filter 自行拼字符串（这是当前一个隐性问题：`error_emit::send_error` 调用方分散）。

#### ADR-G1-6 Thinking signature 错触发整流（E6）

与 codec C3 联动：codec 在解析响应流时若发现 thinking signature 不匹配（OpenAI 格式但缺 `</think>`、anthropic 格式但 `signature` 字段坏掉等），抛 `ThinkingSignatureMismatch` 错误。

整流策略：
1. 流尚未首 chunk → 走 R-F stream probe，触发 failover；
2. 流已首 chunk → 不重试，直接发 `event: error` 给客户端；不喂 K-A cooldown（这是协议层错误，不是 key 健康问题）。

#### ADR-G1-7 KEK 轮换路径（SE3，错误恢复型）

KEK 轮换不是"日常 KMS 操作"，而是"灾难恢复 / 合规轮换 / 泄露应急" 的运维路径，本质上是**带状态机的错误恢复**：

```
Healthy(KEK_v1)
   ↓ admin: rotate_kek(KEK_v2, dual_read=true)
DualRead(KEK_v1+KEK_v2)
   ↓ background: re-encrypt all entities with KEK_v2
DualReadComplete
   ↓ admin: drop(KEK_v1)
Healthy(KEK_v2)
```

接口（`gw-gateway::kms`）：
- `enter_dual_read(kek_v2: SecretBytes32)`：进入 dual-read，新写都用 v2；
- `kick_off_reencrypt()`：后台任务（Tokio task on bootstrap runtime），分批扫 EntityStore 重加密；
- `drop_old_kek()`：v1 状态满足（重加密完成 + 一段静默期）后切换。

**不做**：自动轮换 / 定时轮换 / 多版本并存超过 2 个。

### 1.3 数据流

```text
upstream response (or local error)
   │
   ▼
classify_local / classify_upstream  ──┐    (gw-core::error::classify)
   │                                  │    返回 ClassifierOutcome
   ▼                                  │
ErrorHandler.fanout(outcome) ◄────────┘
   │  KeyHealthListener     → gw-health::apply_cooldown / record
   │  RateLimitFeedback     → gw-auth::ratelimit (RL-Adaptive)
   │  OAuthListener         → gw-adapter::oauth (K-C)
   │  CacheInvalidate       → gw-store::EntityStore
   │  MetricsListener       → gw-gateway::observability
   │  LedgerListener        → gw-ledger
   ▼
render_error_to_client(client_protocol, passthrough_body, class)
   │
   ▼
respond + suppress_error_log(if intentional short-circuit)
```

### 1.4 与 R-E（failover）的边界

- R-E 决定"要不要换 key 重试 / 跨 vendor"，**消费** ClassifierOutcome 的 `retryable` 字段；
- § 1 不做重试；§ 1 只确保 outcome 正确分类、副作用扇出正确。
- R-E 重试时，每次新尝试都重新走 § 1 编排（每跳一条 outcome）。

### 1.5 验收标准（acceptance）

1. `cargo test -p gw-proxy --test error_orchestration` 覆盖：
   - 401 from upstream → ErrorClass=AccountBlocked + KeyHealthListener 写 cooldown 1h + OAuthListener 收到 mark_blocked + 客户端拿到 vendor 形态 401 错误体；
   - 429 with `retry-after: 30` → ErrorClass=RateLimitedUpstream(Rpm) + RateLimitFeedback observe(30s) + KeyHealthListener 写 cooldown 30s + 客户端拿到 429；
   - codec parse fail → ErrorClass=ProtocolError + 不写 cooldown + 不动 RL + 客户端拿到 502 + protocol_error 日志；
   - resilience: Redis 不可达 → fallback_freeze_used 自增 + 请求成功完成（不被 Redis 拖死）；
   - KEK rotate dual-read：旧 KEK 加密 entity 仍能解 + 新写 entity 用新 KEK + 重加密任务到达后 drop_old_kek 成功。
2. listener 注册顺序固定，引入新 listener 必须更新 `scripts/check-invariants.sh` 中 `error_listener_order` lint。
3. 当前 `gw-proxy::error_emit::send_error` 单点保留作为底层发包工具，但**调用方收敛**到 § 1 的 handler 出口；filter 内部不再直接调用。

---

## 2. 能力与路由参数（Capability & Routing Params）

> **覆盖**：inventory 桶 10 整桶（CP1–CP6）+ 桶 9 AU3 / AU7（key 能力 / 模型权限正则）+ 桶 13 H2（事件驱动 cache invalidation，与 capability 缓存联动）。

### 2.1 背景

"能力解析"是当前 gateway 真正的盲点。Aether `services/capability/resolver.py` 把 5 类信号合一：

1. **模型级 capability**（`models` 表里 `capabilities` 列）；
2. **Key 强制 capability**（ApiKey 上 `force_capabilities: ["thinking", "vision"]` —— AU3）；
3. **Header `X-Require-Capability`**（客户端临时声明）；
4. **Adapter 检测**（运行时探出 vendor key 实际可用，如 codex GPT-5 thinking）；
5. **重试升级**（首次失败若是"能力不足"，下次选 key 时升级 capability requirement）。

最终产出 `RequiredCapabilities` 喂给 routing 候选过滤（CP2 exclusive vs compatible 决定匹配模式）。

模型映射 / 价目 / availability 是同一逻辑层的"模型元数据决策"，统一由 capability 子系统承担。

### 2.2 ADR-G2-1 新建 `gw-capability` 业务 crate

依赖方向：

```
gw-core    ◄── gw-capability
gw-store   ◄── gw-capability   （读 entity:model / entity:api_key.force_capabilities）
gw-capability ◄── gw-routing   （filter chain 的一个 filter 调 resolve）
gw-capability ◄── gw-proxy     （filters::request 阶段拿 RequiredCapabilities）
gw-capability ◄── gw-adapter   （adapter 检测钩子 push 实际能力，并消费 model_mapper）
```

**禁止**：
- `gw-capability → gw-routing / gw-proxy / gw-adapter`（业务 crate 不依赖执行体 / 其他业务）；
- `gw-routing` 直接读 `entity:model`（routing 不能跨过 capability 直访模型元数据，避免重复 cache 层）。

### 2.3 ADR-G2-2 RequiredCapabilities 数据形态

```rust
// gw-capability::types
#[derive(Debug, Clone)]
pub struct RequiredCapabilities {
    /// 集合语义。bitset 在 v1 用 SmallVec<[Capability; 6]>，未来字段稳定后可换 u32 bitset。
    pub set: SmallVec<[Capability; 6]>,
    pub mode: MatchMode,
    /// 来源链（debug 用，会写入 detailed_log）。
    pub provenance: SmallVec<[CapabilityOrigin; 4]>,
}

#[non_exhaustive]
pub enum Capability {
    Vision,
    Audio,
    Video,
    Thinking,
    ToolUse,
    StreamingOnly,
    /// 例：codex Responses 形状必须才能跑
    OpenAiResponsesShape,
    /// 例：vertex_ai 必须用 SA 认证（与 ADR-G3-3 联动）
    VertexAiServiceAccount,
    /// 自由扩展槽，由 vendor adapter 注入（带字符串名，避免每次升级 enum）
    Tagged(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub enum MatchMode {
    /// CP2 exclusive：候选 key 必须**完全等于** required（多一不可）
    Exclusive,
    /// CP2 compatible：候选 key 必须**包含** required（可多）
    Compatible,
}

#[derive(Debug, Clone, Copy)]
pub enum CapabilityOrigin {
    Model,
    KeyForced,
    HeaderRequire,
    AdapterDetected,
    RetryUpgrade,
}
```

### 2.4 ADR-G2-3 Resolver 流水线

```rust
// gw-capability::resolver
pub struct CapabilityResolver {
    entity_store: Arc<EntityStore>,
}

impl CapabilityResolver {
    /// 在 filters::request 阶段调用一次。
    /// 返回 RequiredCapabilities，供 routing 用作 filter 维度。
    pub async fn resolve(&self, req: &ResolveRequest<'_>) -> Result<RequiredCapabilities, GwError>;
}

pub struct ResolveRequest<'a> {
    pub api_key_id: &'a KeyId,
    pub model_alias: &'a str,                    // 客户端请求的 model（未映射）
    pub header_require: Option<&'a [&'a str]>,   // X-Require-Capability
    pub protocol: ProtocolKind,
    /// 是否重试（若是，看 RetryUpgradeHint）
    pub retry_upgrade: Option<&'a RetryUpgradeHint>,
}

pub struct RetryUpgradeHint {
    pub last_failure_class: ErrorClass,           // 来自 § 1
    pub last_required: RequiredCapabilities,
    /// 上次失败如果是 ErrorClass::ProtocolError 且涉及"上游不支持 thinking"
    /// → 升级要求 Capability::Thinking 落到下一候选 key
    pub upgrade_path: SmallVec<[Capability; 2]>,
}
```

resolve 内部执行顺序：

1. 读 `entity:api_key.force_capabilities` → 入集合，origin=KeyForced。
2. 读 `entity:model.capabilities`（按 model_alias，先经 § 2.5 model_mapper） → 并集，origin=Model。
3. `header_require` → 并集，origin=HeaderRequire。
4. `retry_upgrade.upgrade_path` → 并集，origin=RetryUpgrade。
5. mode：默认 Compatible；若 `entity:api_key.match_mode = exclusive` 则 Exclusive；header 不能改 mode（防客户端绕过）。
6. **不**在此处做 adapter 检测：检测时机在 `routing::filter` 拿到候选 key 后，对每个候选 key 调用 `AdapterDetectHook`（§ 2.7）。

### 2.5 ADR-G2-4 model_mapper（CP3）

```rust
// gw-capability::mapper
pub struct ModelMapper {
    entity_store: Arc<EntityStore>,
}

pub struct MappedModel {
    /// 客户端原始 model 字符串
    pub user_alias: String,
    /// vendor / vendor_key 上实际下发的 model
    pub upstream_model: String,
    /// 决策依据（用于 detailed_log）
    pub source: MappingSource,
}

pub enum MappingSource {
    /// entity:model_mapping 表命中
    EntityRow,
    /// 未命中，原样透传
    Identity,
    /// 多 vendor key 上同 alias 不同 upstream_model：选 key 时由 routing
    /// 二次确定。这里返回 PerKey
    PerKey,
}

impl ModelMapper {
    pub async fn map_for_routing(
        &self,
        user_alias: &str,
        protocol: ProtocolKind,
    ) -> MappedModel;

    /// routing 已选定 key 后，根据 vendor_key 上的覆盖再决定一次。
    pub async fn map_for_dispatch(
        &self,
        user_alias: &str,
        chosen_key: &VendorKey,
    ) -> String;
}
```

`entity:model_mapping`（`gw-store::entity` 新增）：

| 字段 | 说明 |
|---|---|
| `id` | uuid |
| `user_alias` | 客户端可见 model 名 |
| `protocol` | OpenAI / Anthropic / Gemini |
| `default_upstream_model` | 默认 upstream model |
| `vendor_key_overrides` | `Map<VendorKeyId, upstream_model>` |
| `availability` | `enabled / disabled / shadow_only`（CP4 复用此字段） |
| `pricing_strategy_ref` | 弱引用 entity:pricing_strategy（CP6 用，**Rust 不解析公式**，仅快照传给 Go 计费） |

### 2.6 ADR-G2-5 model_availability（CP4）

availability 的判定**不**是 capability 子系统的副业，而是 ModelMapper.map_for_routing 路径上的硬过滤：

```rust
match availability {
    Enabled => continue,
    Disabled => return Err(GwError::ModelDisabled { user_alias }),
    ShadowOnly => {
        if !req.is_internal_shadow_caller() {
            return Err(GwError::ModelDisabled { user_alias });
        }
    }
}
```

ShadowOnly 是为新模型灰度引入的：内部测试 ApiKey 可用，对外用户拿到 ModelDisabled。判定 `is_internal_shadow_caller` 走 `entity:api_key.flags.allow_shadow_models`。

### 2.7 ADR-G2-6 Adapter 检测钩子

```rust
// gw-capability::adapter_hook
pub trait AdapterCapabilityHook: Send + Sync {
    /// 在 routing 候选过滤阶段被调，针对 (user_alias, vendor_key) 返回该 key
    /// 实际能不能跑这次请求需要的 capabilities。
    /// fast path：纯查 entity:vendor_key.detected_capabilities；
    /// slow path：（极少）真的访问上游 / 调 K-D probe。
    fn detect(&self, ctx: &DetectCtx<'_>) -> CapabilityDetectResult;
}

pub enum CapabilityDetectResult {
    /// 满足 required
    Match,
    /// 缺某个能力
    Missing(SmallVec<[Capability; 2]>),
    /// 探不出来（例：codex 没跑过这个模型，让 routing 把它当 unknown 排末位）
    Unknown,
}
```

注册：每个 adapter 在 `AdapterRegistry::register` 时附带一个 `Option<Arc<dyn AdapterCapabilityHook>>`；多数 adapter 的 hook 退化为"读 entity:vendor_key.capabilities"。codex / vertex_ai 等可注入主动检测逻辑。

### 2.8 ADR-G2-7 重试升级（CP1 末段）

R-E（已有 spec）在执行重试时调用 `CapabilityResolver::resolve(.. retry_upgrade=Some(hint))`。R-E 决定**何时**升级（基于 § 1 ClassifierOutcome），capability 子系统决定**升级到什么**（基于 hint.upgrade_path）。

升级表（v1 默认，可由 entity 配置覆盖）：

| 上次错误类型 | 升级路径 |
|---|---|
| ProtocolError + thinking 不支持 | + Capability::Thinking |
| ProtocolError + tool_use 失败 | + Capability::ToolUse |
| ProtocolError + vision 失败 | + Capability::Vision |
| AccountBlocked + ApiFormat=anthropic | （routing 不重试 capability，而是过滤 vendor）—— 不在 capability 升级范围 |

### 2.9 ADR-G2-8 model_permissions 正则 + ReDoS 防护（AU7）

ApiKey 的 model 白名单当前是字面匹配。AU7 要求支持正则，但必须防 ReDoS（Aether 用 100ms 超时 + `regex` crate 而非 `pcre`）。

```rust
// gw-auth::model_permissions
pub struct ModelPermission {
    /// 编译后的正则。编译时 reject 任何含回溯型 (.*)+ / 嵌套量词 / 命名捕获组等模式。
    pub regex: regex::Regex,
    /// 用于失败日志（便于运维定位错误条目）
    pub raw_pattern: String,
    pub effect: PermissionEffect,
}

pub enum PermissionEffect { Allow, Deny }

pub fn check_model_against_permissions(
    model: &str,
    perms: &[ModelPermission],
) -> bool;
```

防护手段（v1 三层）：
1. **编译时**拒绝 `regex` crate 不支持的危险模式（已经天然不支持回溯）；
2. **运行时**强制 100ms `set_size_limit` + match 复杂度上限（依赖 `regex::RegexBuilder::dfa_size_limit`）；
3. **熔断**：单个 ApiKey 上若 100 次匹配中 10 次超时，整 ApiKey 的 perm 集合标 `permission_disabled` 状态写 EntityStore（被检测到的 ApiKey 直接拒绝请求），同时报 metrics + alert。

### 2.10 ADR-G2-9 upstream model fetcher（CP5）

定期从 vendor 拉 model list，与 `entity:model_mapping` 对账。

**这是 Go 的事，不是 Rust 的事**：fetcher 跑在 Go 后端定时任务，结果写 `entity:model` + `entity:model_mapping`。Rust 仅消费。

本 spec 不实施 fetcher，但**约定 entity 字段**（fetcher 写、Rust 读），见 § 2.5 ADR-G2-4 `entity:model_mapping` 表结构。

> 与 inventory CP5 "可下沉 Go" 一致；本节落入 § 2 仅是为了把 model 元数据闭环讲清楚。

### 2.11 ADR-G2-10 pricing_strategy（CP6）

Rust 端**不解析公式**（已是不变量：billing 归 Go）。但 routing 在选 key / 选 model 时可能需要"成本维度"信号——这通过 `entity:pricing_strategy_snapshot` 给出，由 Go 端定期写入：

```text
entity:pricing_strategy_snapshot:
  key: (model_id, vendor_id)
  value:
    base_input_micro_per_token: u64
    base_output_micro_per_token: u64
    last_updated_ms: u64
```

R-AB 的 `cost_first` 维度直接读这个 snapshot。**v1 不**让 Rust 计算"加权平均成本"——只读单点。

### 2.12 ADR-G2-11 事件驱动 cache invalidation（H2）

EntityStore 现在的失效是被动 TTL，model / api_key / vendor_key 改了之后要等 cache 自然过期。Aether 走"`events:entity` stream + 主动失效"。

接口：
```rust
// gw-store::entity_store
impl EntityStore {
    /// 由 stream_consumer 在收到 events:entity 帧时调。
    pub fn invalidate(&self, kind: EntityKind, id: &EntityId);

    /// 全局失效（KEK rotate / 配置 reload 用）
    pub fn invalidate_all(&self);
}
```

stream:
- `events:entity`，frame：`{kind: "model"|"api_key"|"vendor_key"|"model_mapping"|"pricing_snapshot", id, op: "upsert"|"delete", ts}`。
- 由 Go 端写入；Rust 端 `StreamConsumer` 订阅 + fan out 到 EntityStore + 必要时 fan out 到 § 1.3 的 CacheInvalidateListener（清 capability resolver 内部 LRU）。

### 2.13 验收标准

1. `cargo test -p gw-capability --test resolve` 覆盖：
   - model 仅声明 vision，header 加 thinking → 集合是 {vision, thinking}，origin 链含 Model + HeaderRequire；
   - ApiKey force_capabilities=[audio]，model 不带 audio → 集合包含 audio，routing 找不到匹配 vendor key 时 ErrorClass=Internal+capability 失败；
   - retry_upgrade hint 含 Thinking → 第二次 resolve 集合多 thinking。
2. `cargo test -p gw-capability --test mapper` 覆盖：identity / EntityRow / PerKey 三种 MappingSource。
3. `cargo test -p gw-auth --test model_permissions_redos`：构造一个会让 PCRE 卡死但 regex crate 不支持的模式，验证编译期 reject；构造一个能匹配但量级巨大的输入，验证 100ms 超时熔断。
4. `cargo test -p gw-store --test events_entity_invalidate`：events:entity upsert → invalidate 命中。
5. `scripts/check-invariants.sh` 加：
   - `gw-routing` 不许 import `entity::model`（必须经 capability）；
   - `gw-capability` 不许依赖 `gw-routing / gw-proxy / gw-adapter`。

---

## 3. 鉴权完善（Authentication Hardening）

> **覆盖**：inventory 桶 9 残项 AU1 / AU2 / AU4 / AU5（AU3 / AU6 / AU7 已分别在 § 2 / 现有实现 / § 2.9 处理）+ 桶 15 SE2 / SE5（与 AU4 / AU2 重复，引用即可）。

### 3.1 ADR-G3-1 JWT iss / aud / nbf 完整校验（AU1）

`gw-auth::jwt` 当前仅校验签名 + exp。补三段：

```rust
pub struct JwtValidator {
    expected_iss: SmallVec<[String; 2]>,   // 多 issuer 兼容
    expected_aud: SmallVec<[String; 2]>,
    leeway_secs: u32,                       // 默认 30s（处理时钟漂移）
    require_nbf: bool,                      // 默认 true
}
```

校验顺序：签名 → exp → nbf → iss → aud。任一失败即 `GwError::AuthInvalidJwt(reason)`，**reason 不带原始 token**（避免日志泄露）。

`expected_iss` / `expected_aud` 从 `entity:auth_config` 读取，由 Go 端配置。

### 3.2 ADR-G3-2 JWT 黑名单（AU2 / SE5）

撤销态走 Redis Set，key 命名 `auth:jwt_blacklist:<jti>`，TTL = token 剩余有效期 + leeway。

```rust
pub trait JwtBlacklist: Send + Sync {
    async fn is_revoked(&self, jti: &str) -> Result<bool, GwError>;
    async fn revoke(&self, jti: &str, until_unix_s: u64) -> Result<(), GwError>;
}
```

**fail-closed / fail-open 开关**（SE5）：

```rust
pub enum BlacklistFailMode {
    /// Redis 不可达时 → 视为 revoked（拒绝请求），最严格
    FailClosed,
    /// Redis 不可达时 → 视为未撤销（放行），可用性优先
    FailOpen,
}
```

默认 FailClosed。运行期可由 `entity:auth_config.blacklist_fail_mode` 切换；切换事件写 `events:entity` 触发 EntityStore invalidate。

**性能要求**：Redis EXISTS 单次查询，`HotPath` 上以 fred pipeline 合并到与 ApiKey 校验同一 round-trip。

### 3.3 ADR-G3-3 HMAC 签名强制（AU4 / SE2）

当前 HMAC 签名是**可选**：客户端带就校，不带就过。AU4 要求按 ApiKey 强制。

`entity:api_key.signature_required: bool` + `entity:api_key.signature_algo: HmacSha256 | HmacSha512`。

```rust
// gw-auth::signature
pub fn verify_request_signature(
    api_key: &ApiKey,
    headers: &dyn HeadersAccessor,
    body_digest: &[u8; 32],
) -> Result<(), GwError>;
```

签名算法（沿用 Aether 现有规范，避免客户端重写）：

```text
canonical = format!("{method}\n{path}\n{x-timestamp}\n{body_sha256_hex}")
signature = base64(hmac_sha256(api_key_secret, canonical))
```

防重放：`x-timestamp` 与服务器时钟差 > 300s 拒绝（leeway 同 JWT）。
防 body 替换：`body_sha256_hex` 由 gateway 在 `request_body_filter` 内计算后再走签名校验——**这意味着签名校验时机要从 `filters::request` 推迟到 body 完整后**。后果：鉴权流水线分两阶段，header-only 校验 + body-after 校验。在 § 8 实施次序中标记。

签名失败：`GwError::AuthSignatureInvalid`，**不**告知客户端"是哪一步失败"（防止 oracle 攻击）。

### 3.4 ADR-G3-4 management_tokens 独立通道（AU5）

management token 是"受限管理面调用 gateway 自身管理 API"用的，与业务 API key **完全独立**：

- 路由：仅 `/admin/*` 路径接受；
- 校验：独立 entity `entity:management_token`，独立 Redis 命名空间 `mgmt:*`；
- 权限：作用域是粗粒度 `mgmt:freeze:write` / `mgmt:health:read` 等；
- 失效：可按 token 撤销（与 § 3.2 黑名单分开，不复用）。

**v1 不实施完整管理面**——本节只占位字段 + 鉴权骨架，让管理路径在 `gw-proxy::filters::request` 早分流：

```rust
fn is_management_path(path: &str) -> bool {
    path.starts_with("/admin/")
}
```

Management 路径上**禁用** ApiKey + JWT，仅接受 `Authorization: Mgmt <token>`。具体管理 API 端点设计不在本 spec 范围（待 Go 端管理面 spec 拍）。

### 3.5 验收标准

1. `cargo test -p gw-auth --test jwt_validation`：合法/iss-错/aud-错/nbf-未到/exp-过期/签名错 6 用例全过；reason 不含 token 原文。
2. `cargo test -p gw-auth --test jwt_blacklist`：revoked 拒绝；FailClosed 模式下 Redis down 拒绝；FailOpen 模式下 Redis down 放行。
3. `cargo test -p gw-auth --test signature_required`：required=true 时缺签名 401；required=true 时签名错 401（统一错误码，不分细节）；required=false 时缺签名仍放行。
4. `cargo test -p gw-proxy --test mgmt_path_routing`：`/admin/foo` 走 management 通道，business ApiKey 拒绝。

---

## 4. Codec 残项（Codec Tail）

> **覆盖**：inventory 桶 1 残项 C6 / C7 / C8 / C9。已立项的 S1~S5 不动；本节只填 S 系列没接的"非主流但生产必需"形状。

### 4.1 ADR-G4-1 sync ↔ stream 桥接（C6）

场景：
- 客户端 `stream=false`，但 vendor key 是 codex / antigravity，**只**支持 stream → 网关 fake stream→ accumulate→ 整包返回；
- 客户端 `stream=true`，但选中的 vendor 走非流式 API（罕见，主要是某些 vertex_ai endpoint）→ 网关 fake non-stream→ 拆 chunk 模拟 SSE。

接口落在 `gw-codec::stream_policy`：

```rust
pub enum UpstreamStreamPolicy {
    /// 客户端要 sync，上游也能 sync：直送
    PassThroughSync,
    /// 客户端要 sync，上游强 stream：网关 accumulate
    AccumulateUpstreamStream,
    /// 客户端要 stream，上游也能 stream：直送
    PassThroughStream,
    /// 客户端要 stream，上游强 sync：网关 fake stream
    SimulateStreamFromSync,
}

pub fn decide_stream_policy(
    client_wants_stream: bool,
    upstream_caps: UpstreamStreamCapability,
) -> UpstreamStreamPolicy;

pub struct UpstreamStreamCapability {
    pub supports_sync: bool,
    pub supports_stream: bool,
}
```

`UpstreamStreamCapability` 由 adapter 通过 trait v2 的 `transport_profile()` 返回。

**注意 v1 已知 gap**：CLAUDE.md 说"非流直接 400"是因为 codec 的 parse_response 全 unimplemented。S4 落地后才有 `AccumulateUpstreamStream`；本节仅定**接口**，实施排序在 § 8。

### 4.2 ADR-G4-2 内容编码处理（C7）

Pingora 不自动解 br/gzip/zstd（client 侧 br 也常见）。需要在两侧加：

入站（客户端 → 网关）：
- 客户端 `Content-Encoding: gzip|br|zstd` → 网关在 `request_body_filter` 解码后才走 codec parse；
- v1 仅支持 gzip / br；zstd 留 trait 槽。

出站（网关 → 上游）：
- adapter 通过 trait v2 表态"我希望 body 用 br"——`AdapterCtx::set_outbound_encoding(Encoding::Br)`；
- gateway 在 `upstream_request_filter` 完成压缩。

回程（上游 → 网关）：
- Pingora upstream client 会按 `Accept-Encoding` 协商；网关请求时显式声明 `Accept-Encoding: identity, gzip, br`，**不**接受 zstd（v1）；
- 上游 SSE 强制 identity（br/gzip 与流不友好）。

回程（网关 → 客户端）：
- 客户端 `Accept-Encoding` 含 br/gzip → 网关压缩；SSE 必须 identity。

依赖：
- `flate2` (gzip) 已在 Cargo.toml 通过 aether-proxy 依赖出现，本网关 workspace 单独引入；
- `brotli`：新增 workspace dep。

**性能基线**：codec passthrough bench (`gw-codec/benches/passthrough`) 加入压缩/解压用例，opt-level=3 下解压 1MB body p50 < 5ms。

### 4.3 ADR-G4-3 OpenAI Responses API（C8）

codex adapter（A-T5）依赖 `/v1/responses` 形状，与 `/v1/chat/completions` **形状完全不同**（input 是 ContentPart 列表，output 是 stateful response objects）。

S2 已经把 ContentPart 多模态做到 wire 层，但 Responses 形状的请求侧 / 响应侧 wire 类型尚未声明。本节为：

- 在 `gw-codec::openai` 下加 `responses/` 模块：`request.rs / response.rs / stream_chunk.rs`，对应 wire 类型 `OpenAiResponsesRequest` / `OpenAiResponsesResponse` / `OpenAiResponsesStreamChunk`；
- 注册进 `WireAny` 枚举（S1 已建好骨架）：
  ```rust
  pub enum WireAnyRequest {
      OpenAiChat(OpenAiChatRequest),
      OpenAiResponses(OpenAiResponsesRequest),
      Anthropic(AnthropicRequest),
      Gemini(GeminiRequest),
      // ...
  }
  ```
- 注册进 `wire_convert_matrix!`（S1 proc-macro 骨架），声明 OpenAiResponses ⇄ {OpenAiChat, Anthropic, Gemini} 四向（含跨形状）。

**Capability 联动**：当客户端走 `/v1/responses` 形态时，capability 子系统（§ 2）自动加 `Capability::OpenAiResponsesShape` 到 RequiredCapabilities，路由只选支持该形状的 vendor key。

### 4.4 ADR-G4-4 Gemini Files API（C9）

Gemini 多模态用"先上传 → 拿 file_uri → 在请求里引用"两步走，不能像 OpenAI 那样塞 base64。

- 网关需要中转上传 `POST /upload/v1beta/files`（resumable upload，分片）；
- 上传后把 `file_uri` 缓存在 `entity:gemini_file_mapping` 表（**Rust 写**，因为这是请求路径上动作，不是配置数据），key=（user_id, sha256(content)），TTL=48h（Gemini 默认）。

接口：
```rust
// gw-codec::gemini::files
pub trait GeminiFilesGateway: Send + Sync {
    async fn ensure_uploaded(
        &self,
        ctx: &UploadCtx<'_>,
        bytes: &[u8],
        mime: &str,
    ) -> Result<GeminiFileRef, GwError>;
}

pub struct GeminiFileRef {
    pub file_uri: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub expire_unix_s: u64,
}
```

`ensure_uploaded` 内部：
1. sha256 → 查 `entity:gemini_file_mapping`；
2. 命中 + 未过期 → 直接返回；
3. 否则上传 → 写 mapping → 返回。

并发上传去重：单 sha256 走 in-process `OnceCell<Pin<Box<...>>>` 锁；进程间不去重（v1 不上 Redis 全局锁）。

写表是**少见**的 Rust 写 entity 场景，但 mapping 是请求路径上动作的副作用 + 强 TTL，归 Rust 写更合理。`gw-store::entity_store` 提供 `upsert_gemini_file_mapping(...)`。

### 4.5 验收标准

1. `cargo test -p gw-codec --test stream_policy_decision`：四种策略 truth table 全覆盖。
2. `cargo bench -p gw-codec --bench compression`：gzip / br 解压 1MB p50 < 5ms。
3. `cargo test -p gw-codec --test openai_responses_roundtrip`：Responses ⇄ Chat ⇄ Anthropic 三向 round-trip 不丢字段（S2 已有的 Anthropic 多模态测试模板复用）。
4. `cargo test -p gw-codec --test gemini_files_dedup`：同 sha256 上传 2 次仅一次实际 HTTP；TTL 过期后自动重传。

---

## 5. 隧道节点子系统（Tunnel Node Subsystem）

> **覆盖**：inventory 桶 12 整桶（TN1–TN6）。

### 5.1 背景

主网关在墙内时需要海外出口。Aether 的解决方案是 `aether-proxy`（VPS 上跑）+ `aether-hub`（中央 frame router）+ `services/proxy_node` 服务编排三件套，自带 TUI 配置 + systemd + safe DNS + target filter。

本 spec 只设计**网关侧的 tunnel client**，与 hub / proxy 节点的 frame protocol；hub / proxy 节点的实现复用 Aether 现有 `aether-hub` / `aether-proxy` 二进制（**不重写**），网关侧适配它们的协议。

### 5.2 ADR-G5-1 新建 `gw-tunnel-client` 业务 crate

理由：
- 规模独立（隧道协议、heartbeat、连接池、frame 编解码）；
- 触发条件可选（部署时不需要隧道则整 crate 关掉）；
- v2 集成约束：不与 `gw-proxy` 互依。

依赖（**业务→业务**依赖在此 spec 显式注册，满足 v2 集成 § 3 C-1）：
```
gw-core   ◄── gw-tunnel-client
gw-store  ◄── gw-tunnel-client   （读 entity:proxy_node 节点列表）
gw-tunnel-client ◄── gw-adapter  （adapter 通过 transport_profile 选 tunnel；显式 business→business 边）
```

**execution 层不归此 crate**——`gw-proxy` 调 tunnel-client 时通过 trait 注入，不直接依赖 crate（避免 execution → tunnel-client → adapter 的回环）。

具体形态：tunnel-client 作为 `reqwest::Client` 的 custom transport / connector 注入到 adapter 的 HTTP client；adapter 通过 `AdapterTransport` trait 间接选用。

### 5.3 ADR-G5-2 Frame protocol（TN2）

复用 `aether-hub/src/protocol.rs` 现行帧格式（不变更，让 hub / proxy 节点不改）：

```text
Frame:
  magic(2) | version(1) | msg_type(1) | flags(2) | stream_id(4) | length(4) | payload(length)

msg_type:
  0x01 OPEN
  0x02 DATA
  0x03 CLOSE
  0x04 PING
  0x05 PONG
  0x06 ERROR
  0x10 CONTROL_AUTH
  0x11 CONTROL_NODE_INFO
```

Rust 侧用 `bytes::Bytes` 零拷贝解码；编码用 `BufMut`。`stream_id` 是 32-bit，单连接最大 4G 流——单连接连续打开关闭不重用 id（避免 race），重用阈值 = u32::MAX 后 reset 连接。

### 5.4 ADR-G5-3 Tunnel client 形态

```rust
// gw-tunnel-client::client
pub struct TunnelClient {
    hub_url: Url,
    auth_token: SecretString,
    pool: ConnectionPool,
    heartbeat_interval: Duration,
    backoff: ExponentialBackoff,
}

impl TunnelClient {
    /// 由 adapter transport 调；返回一个流式 body（与 reqwest::Body 兼容）。
    pub async fn dispatch(&self, req: TunnelRequest) -> Result<TunnelResponse, TunnelError>;
}
```

`ConnectionPool`：
- 多条到 hub 的长连接，按节点指派 round-robin；
- 每连接最多 256 路并发流（match aether-hub 默认）；
- 心跳 PING/PONG，30s 一发，3 次未收 PONG 即重连；
- 重连退避：指数 1s → 60s 上限，jitter ±20%。

### 5.5 ADR-G5-4 proxy_nodes 节点选择（TN4）

`entity:proxy_node`（**Go 写、Rust 读**）：

| 字段 | 说明 |
|---|---|
| `id` | uuid |
| `name` | 易读名 |
| `region` | "us-east-1" / "ap-tokyo-1" 等 |
| `hub_url` | 该节点对应 hub 入口 |
| `enabled` | 是否启用 |
| `allowed_provider_ids` | 节点白名单（仅这些 provider 可走此节点）|
| `allowed_vendor_key_ids` | 节点细粒度白名单 |
| `latency_score_ms` | hub 上报的最近 RTT，**Rust 通过 events:tunnel 流读** |

选节点接口：
```rust
// gw-tunnel-client::resolver
pub fn resolve_tunnel(
    ctx: &TunnelResolveCtx<'_>,
) -> TunnelResolution;

pub struct TunnelResolveCtx<'a> {
    pub provider_id: &'a ProviderId,
    pub vendor_key_id: &'a VendorKeyId,
    /// adapter 通过 transport_profile 给出的偏好（例：claude_code 偏好低延迟）
    pub preference: TunnelPreference,
}

pub enum TunnelResolution {
    /// 走 tunnel
    Use(ProxyNodeId),
    /// 直连（无可用 tunnel 或不需要）
    Direct,
}
```

选择策略 v1：filter (enabled + allowed_*) → sort by latency_score_ms → pick first；同分时 round-robin。

### 5.6 ADR-G5-5 safe_dns（TN5）

DNS 污染防护：
- 内嵌 DoH client（`hickory-dns` 或 `trust-dns-resolver`），上游 DoH = `https://1.1.1.1/dns-query` / `https://8.8.8.8/dns-query`，启动期可选；
- 解析结果带 TTL，本地 LRU 缓存（capacity 4096，max TTL 600s）；
- **A 记录 IP 黑名单**：`10/8 / 127/8 / 169.254/16 / 私网段` 全 reject（防 SSRF 间接走 tunnel 攻击网关 metadata）；
- **同时**存在 hosts override 配置（`entity:safe_dns_override`），可强制 vendor 域名解析到指定 IP（例：上游 IP 直连绕过 vendor CDN）。

`gw-tunnel-client::safe_dns` 同时给 § 4 / § 5 / 直连 reqwest client 用——**不限**于 tunnel 路径。直连场景下也走 safe_dns 是对 SSRF 的额外一层防护。

### 5.7 ADR-G5-6 target_filter（TN6）

过滤"网关或 tunnel 能连什么":
- 默认 deny；
- whitelist by vendor base url + port（443 only）；
- 例外：health probe 允许 80（少数 endpoint 仅 HTTP）；
- whitelist 由 `entity:target_filter` 提供，初始数据由部署时种入。

```rust
// gw-tunnel-client::target_filter
pub fn assert_target_allowed(target: &Url) -> Result<(), GwError>;
```

被违反的所有出站请求统一 `ErrorClass::Internal` + metric `gw_target_filter_denied_total`。

### 5.8 ADR-G5-7 部署形态

- `gw-tunnel-client` **不**编入 `gw-gateway` 二进制默认路径——通过 cargo feature `tunnel`（默认 off）启用；
- 启用时，bootstrap 阶段读 `entity:proxy_node`，初始化 TunnelClient，注入到 adapter HTTP client 工厂；
- 关闭时，所有 `TunnelResolution` 永远是 `Direct`，crate 整体不编译进二进制。

`hub` / `aether-proxy` **不进**本 workspace —— 两者作为外部部署组件存在，与 Aether 现有版本兼容（不强求 Aether 升级才能用）。

### 5.9 验收标准

1. `cargo test -p gw-tunnel-client --test frame_codec`：所有 msg_type 编解码 round-trip + 边界 length。
2. `cargo test -p gw-tunnel-client --test pool_heartbeat`：模拟 PONG 丢失 3 次后自动重连；重连退避 jitter 落在 [0.8x, 1.2x]。
3. `cargo test -p gw-tunnel-client --test resolver_filter`：allowed_vendor_key_ids 命中过滤；latency_score 排序；同分 RR。
4. `cargo test -p gw-tunnel-client --test safe_dns_redirect_block`：DoH 返回 169.254.169.254 → 拒绝（防 GCE / EC2 metadata 攻击）。
5. `cargo test -p gw-tunnel-client --test target_filter_strict`：未在白名单的 host 直连和 tunnel 都被拒绝。
6. workspace `cargo build --no-default-features` 不编译 `gw-tunnel-client`，二进制大小 - 至少 800KB（验证 feature gate 真切断）。

---

## 6. 可观测性补齐（Observability Tail）

> **覆盖**：inventory 桶 14 残项 O1 / O2 / O3 / O5。其余 O4 / O6 / O7 / O8 / O9 已有现状或归 Go。

### 6.1 ADR-G6-1 6 类指标补齐（O1）

新增/补齐的 Prometheus 指标族：

| 域 | 指标 | 类型 | labels |
|---|---|---|---|
| codec | `gw_codec_convert_total` | Counter | `direction, source, target, status` |
| codec | `gw_codec_convert_duration_seconds` | Histogram | `direction, source, target` |
| codec | `gw_codec_passthrough_bytes_total` | Counter | `direction` |
| freeze | `gw_freeze_acquire_total` | Counter | `outcome=ok\|denied\|fallback` |
| freeze | `gw_freeze_release_total` | Counter | `outcome=committed\|rolled_back\|leaked` |
| freeze | `gw_freeze_estimate_micro` | Histogram | `model` |
| security | `gw_auth_failure_total` | Counter | `reason=jwt_iss\|jwt_aud\|jwt_nbf\|jwt_revoked\|sig_invalid\|apikey_invalid` |
| security | `gw_target_filter_denied_total` | Counter | `host` |
| security | `gw_redos_timeout_total` | Counter | `api_key_id` |
| kms | `gw_kms_state` | Gauge | `state=healthy\|dual_read\|reencrypting` |
| kms | `gw_kms_reencrypt_progress` | Gauge | （0–1） |
| oauth | `gw_oauth_refresh_total` | Counter | `vendor, outcome=ok\|failed\|skipped_lock_held` |
| oauth | `gw_oauth_token_age_seconds` | Histogram | `vendor` |
| capability | `gw_capability_resolve_total` | Counter | `match=hit\|miss\|upgrade` |
| capability | `gw_capability_unknown_total` | Counter | `vendor` |

**O6 status-class collapse 修复**（inventory 标 Rust 已记录 issue）一并落入：metrics 的 `status` 标签必须落在 `2xx / 3xx / 4xx / 5xx / network_error / canceled` 6 个桶，避免每个状态码都开 cardinality。

### 6.2 ADR-G6-2 codec 转换指标（O2）

- 单点埋点：`gw-codec::convert::WireAnyRequest::convert_to::<B>` 进入/退出 时计时；
- 状态：`ok / parse_fail / mapping_fail / panic`；
- 用 `metrics` crate 的 macros，避免每次 hot path 上 lookup label。

### 6.3 ADR-G6-3 health endpoint + circuit 暴露（O3）

`/healthz` 已有 livez/readyz；新增：
- `/healthz/key/<key_id>?format=anthropic` → 返回 K-A 的 `HealthSnapshot`；
- `/healthz/circuits` → 返回所有 (KeyId, ApiFormat) → state 摘要（仅 admin token 可见，§ 3.4）；
- `/healthz/resilience` → ResilienceState（§ 1.5 ADR-G1-5）。

实现：`gw-gateway::health` 加新 router，仅 8080 端口（与 Pingora 业务面 8000 隔离）。**不暴露**到 9090（metrics 端口语义干净，留给 Prometheus）。

### 6.4 ADR-G6-4 detailed_log + simple_log + 写入时脱敏（O5）

当前 `gw-ledger::log_sink` 是写一行，无分级。Aether 走 detailed/simple 双层 + 写入时脱敏。

```rust
// gw-ledger::log_sink
pub enum LogVerbosity {
    /// 仅元数据：req_id / user_id / model / latency / outcome / cost
    Base,
    /// Base + headers（白名单）+ ErrorClass 链
    Headers,
    /// Headers + body（前 4KB，超过截断）
    Full,
}

pub struct DetailedLogRecord {
    pub req_id: RequestId,
    pub verbosity: LogVerbosity,
    // Base 字段
    pub api_key_id: KeyId,
    pub model_alias: String,
    pub upstream_model: String,
    pub vendor_id: VendorId,
    pub vendor_key_id: VendorKeyId,
    pub status_class: StatusClass,
    pub latency_ms: u32,
    pub error_class: Option<ErrorClass>,
    // Headers 字段
    pub req_headers_subset: Option<SmallVec<[(String, String); 8]>>,
    pub resp_headers_subset: Option<SmallVec<[(String, String); 8]>>,
    pub error_chain: Option<SmallVec<[ErrorClass; 4]>>,
    // Full 字段
    pub req_body_excerpt: Option<Bytes>,    // 截到 4KB
    pub resp_body_excerpt: Option<Bytes>,
}
```

verbosity 由 `entity:logging_config.verbosity` 决定，可 per-ApiKey override。

写入时脱敏（**进 sink 之前**做，不靠下游消费者）：
- `req_headers_subset`：从白名单中取（`x-stainless-*` / `user-agent` / `accept-encoding` 等）；显式禁出 `authorization` / `x-api-key` / `cookie`；
- body 脱敏：以 JSON 字段路径黑名单（`messages[*].content[*]` → "<redacted-text-N-chars>"，但保留 `role` / `tool_calls.name`）；
- 二进制（图片 / 音频 base64） → 替换成 `<media:type=image/png;size=12345>`；
- 兜底：单条 detailed log 序列化后 > 64KB 整条丢弃 + 计 `gw_detailed_log_oversized_total`。

写出口：
- `detailed_log`：写 `usage:detailed` Redis stream，由 Go 异步消费持久化；
- `simple_log`：仅 stderr / file（已有）。

### 6.5 验收标准

1. `cargo test -p gw-gateway --test metrics_endpoint`：列出新增 14 类指标，`/metrics` 全部出现且 cardinality < 200（合理上限）。
2. `cargo test -p gw-gateway --test health_endpoint_admin_only`：`/healthz/circuits` 不带 mgmt token → 401。
3. `cargo test -p gw-ledger --test redaction`：authorization header 永不出现在 detailed log；JSON body 中文长 message 被截 + 字符数标注；二进制 media 被替换。
4. `cargo bench -p gw-ledger --bench redaction`：单条 1MB body 脱敏 p50 < 2ms。

---

## 7. 与已有 spec / inventory 的引用矩阵

| 本 spec 节 | 直接对应 inventory 条目 | 与已有 spec 的依赖 |
|---|---|---|
| § 1.1–1.6 | 桶 7 E1–E6 | 消费 K-A `apply_cooldown` / `record`；消费 K-B `error_analyzer`（不重新引 trait）；消费 K-C `mark_refresh_failed`；消费 RL-Adaptive `observe`；为 R-E 提供 ClassifierOutcome.retryable |
| § 1.7 | 桶 15 SE3 | 复用 `gw-gateway::kms` 现状 + 加 dual-read 状态机 |
| § 2.1–2.7 | 桶 10 CP1 / CP2 + 桶 9 AU3 | 喂入 R-AB / R-D 的 filter chain（routing 选 key 时调 capability） |
| § 2.5 / 2.6 | 桶 10 CP3 / CP4 | 新 entity:model_mapping，Go 写 / Rust 读 |
| § 2.7 | 桶 10 CP5 | **归 Go 实施**，本节仅约定 entity 字段 |
| § 2.8 | 桶 9 AU7 / 桶 15 SE4 | 在 `gw-auth::model_permissions` 落地，与 `gw-auth::merge` 协作 |
| § 2.9 | 桶 13 H2 | 扩 `gw-store::EntityStore` invalidate；扩 `gw-store::stream_consumer` 订阅 events:entity |
| § 2.10 | 桶 10 CP6 | 仅约定 snapshot entity；R-AB 的 cost_first 维度直接消费 |
| § 3.1 | 桶 9 AU1 | `gw-auth::jwt` 扩 |
| § 3.2 | 桶 9 AU2 / 桶 15 SE5 | `gw-auth::jwt` 加 BlacklistFailMode |
| § 3.3 | 桶 9 AU4 / 桶 15 SE2 | `gw-auth::signature` 强制路径；与 `request_body_filter` 时序耦合 |
| § 3.4 | 桶 9 AU5 | 新 entity:management_token；`gw-proxy::filters::request` 早分流 |
| § 4.1 | 桶 1 C6 | `gw-codec::stream_policy` 新增；依赖 S4 落地后才能启用 AccumulateUpstreamStream |
| § 4.2 | 桶 1 C7 | `gw-proxy::filters::{request_body, response_body}` 加压缩；workspace 引 brotli |
| § 4.3 | 桶 1 C8 | `gw-codec::openai::responses` 新模块；进 S1 wire_convert_matrix |
| § 4.4 | 桶 1 C9 | `gw-codec::gemini::files` + entity:gemini_file_mapping |
| § 5 整节 | 桶 12 TN1–TN6 | 新 crate `gw-tunnel-client`；feature gate `tunnel` |
| § 6.1 | 桶 14 O1 / O6 | `gw-gateway::observability` 扩 |
| § 6.2 | 桶 14 O2 | `gw-codec` 内埋点 |
| § 6.3 | 桶 14 O3 | `gw-gateway::health` 扩；与 § 3.4 mgmt token 协作 |
| § 6.4 | 桶 14 O5 | `gw-ledger::log_sink` 重写 + `usage:detailed` stream |

**反向引用**（本 spec 与已有 spec 的对接锚点）：

| 已有 spec | 锚点 |
|---|---|
| K-A § 5（OutcomeKind / CooldownDirective） | § 1.2 ADR-G1-1 ClassifierOutcome.cooldown 直接喂 |
| K-B（vendor 错误钩子） | § 1.2 / § 1.4：本 spec 要求把 K-B 的 `analyze` 返回值扩为 `AnalyzerOutcome { cooldown, enhanced_body, oauth_signal }`，吸收 E4 enhancer 职责；K-B 落地时一并修订 |
| K-C（OAuth） | § 1.3 OAuthListener fan-out 的目标 |
| R-AB（multi-score） | § 2 RequiredCapabilities 是 R-AB filter chain 的一个 filter；§ 2.10 pricing_snapshot 喂 cost_first 维度 |
| R-D（AffinityManager） | 不冲突；capability filter 与 affinity filter 串行 |
| R-E（Failover） | § 1.4 边界讨论；R-E 消费 retryable + § 2.7 ClassifierOutcome 触发 capability upgrade |
| R-F（Stream probe） | § 4.1 SimulateStreamFromSync / § 1.6 thinking signature 重试都依赖 R-F 安全网 |
| RL-Core / RL-Adaptive | § 1.3 RateLimitFeedbackListener 的目标 |
| C-α（UsageRecord） | § 6.4 detailed_log 与 UsageRecord 同源（同一 RequestId）但**双流**（usage:records + usage:detailed） |
| S1（IR + Codegen） | § 4.3 `OpenAiResponsesRequest` 注册进 wire_convert_matrix |
| S2（多模态） | § 4.4 Gemini Files 是 S2 多模态在 vendor 侧的特殊路径 |
| S4（响应 + 跨协议） | § 4.1 AccumulateUpstreamStream 依赖 S4 |
| Adapter A-T0 trait v2 | § 2.7 AdapterCapabilityHook / § 4.1 transport_profile / § 5.2 AdapterTransport 都挂在 trait v2 上 |

---

## 8. 实施次序与不变量

### 8.1 实施次序（topological，**不**是 plan）

```text
phase G-α  错误编排 + 鉴权完善
  G-α.1  ErrorClass + Classifier trait（§ 1.2 ADR-G1-1/2）
  G-α.2  ErrorHandler + listener 注册（§ 1.3 ADR-G1-3）
  G-α.3  error passthrough（§ 1.4 ADR-G1-4，依赖 codec S4 已落地）
  G-α.4  resilience state（§ 1.5）
  G-α.5  thinking signature 整流（§ 1.6，依赖 R-F）
  G-α.6  KEK rotate（§ 1.7）
  G-α.7  JWT iss/aud/nbf + blacklist（§ 3.1 / 3.2）
  G-α.8  HMAC 强制（§ 3.3，要求 request_body_filter 时序调整）
  G-α.9  management_tokens 路径分流（§ 3.4，仅占位）

phase G-β  能力子系统
  G-β.1  ErrorClass / RetryUpgradeHint 已就绪后开
  G-β.2  新建 gw-capability crate（§ 2.2）
  G-β.3  RequiredCapabilities + Resolver（§ 2.3 / 2.4）
  G-β.4  ModelMapper + entity:model_mapping（§ 2.5）
  G-β.5  AdapterCapabilityHook（§ 2.7）
  G-β.6  events:entity invalidate（§ 2.12，依赖 stream_consumer 已稳定）
  G-β.7  model_permissions ReDoS 防护（§ 2.9）
  G-β.8  retry upgrade 表（§ 2.8，与 R-E 协作）

phase G-γ  codec 残项
  G-γ.1  stream_policy 决策表（§ 4.1，仅接口）
  G-γ.2  入站/回程压缩（§ 4.2）
  G-γ.3  OpenAI Responses 形状（§ 4.3，依赖 S1 矩阵宏 + S2 ContentPart）
  G-γ.4  Gemini Files API（§ 4.4）

phase G-δ  可观测
  与 α/β/γ 并行
  G-δ.1  6 类指标 + status-class collapse（§ 6.1）
  G-δ.2  codec 指标（§ 6.2）
  G-δ.3  health endpoint 扩展（§ 6.3，依赖 § 3.4 mgmt token 占位）
  G-δ.4  detailed/simple log + 脱敏（§ 6.4）

phase G-ε  隧道（独立分支，feature gate）
  G-ε.1  新 crate gw-tunnel-client + frame protocol（§ 5.2 / 5.3）
  G-ε.2  ConnectionPool + heartbeat（§ 5.4）
  G-ε.3  resolver + entity:proxy_node（§ 5.5）
  G-ε.4  safe_dns（§ 5.6）
  G-ε.5  target_filter（§ 5.7）
  G-ε.6  cargo feature 串接（§ 5.8）
```

依赖纵剖：

- α 内部线性（α.3 依赖 codec S4，但 S4 已立项，不阻塞 α）；
- β 依赖 α 的 ErrorClass / RetryUpgradeHint；
- γ.1 仅接口，不阻塞，γ.2 / γ.3 / γ.4 独立；
- δ 与 α/β/γ 并行；
- ε 完全独立，不阻塞主路径。

### 8.2 新增不变量（lint）

补入 `scripts/check-invariants.sh`：

| lint | 内容 | 阶段 |
|---|---|---|
| `crate_layering` 扩 | `gw-capability` 加入业务层；禁止依赖 `gw-routing / gw-proxy / gw-adapter / gw-tunnel-client` | G-β |
| `crate_layering` 扩 | `gw-tunnel-client` 加入业务层；禁止依赖 execution；feature off 时不进二进制 | G-ε |
| `no_routing_entity_model` | `gw-routing` 不许 `use gw_store::entity::model::*` | G-β |
| `no_capability_routing` | `gw-capability` 不许 import routing | G-β |
| `error_listener_order` | `gw-proxy::error::handler::register_default_listeners` 注册顺序固定 | G-α |
| `no_redos_pcre` | `gw-auth::model_permissions` 不许引 pcre / fancy_regex / onig | G-β |
| `mgmt_path_strict` | `/admin/*` 路径上 ApiKey/JWT 鉴权器 panic（编译期断言由 const 路径白名单实现） | G-α |
| `tunnel_feature_gate` | `gw-gateway` 默认特性集编译产物**不**含 `gw_tunnel_client::` 符号 | G-ε |
| `detailed_log_no_secret` | `gw-ledger::log_sink` 源码内的 header 白名单 / 黑名单常量必须出现在指定 const 区块（`const REDACT_HEADER_DENYLIST` / `const REDACT_HEADER_ALLOWLIST`）；这两个 const 之外**任何**地方出现 `authorization\|x-api-key\|cookie` 字面量都触发 lint（避免误把 header 值打日志）| G-δ |
| `kms_dual_read_singleton` | KEK rotate 状态机最多 2 KEK 并存 | G-α |

### 8.3 Done 标准（spec 整体）

`scripts/check-invariants.sh` 全 ok + 各节验收测试列表全过 + observability dashboards 引用 § 6.1 全部指标。

---

## 9. 术语速查

| 术语 | 含义 |
|---|---|
| ErrorClass | § 1.2 引入的客户端 / 编排层错误语义枚举 |
| ClassifierOutcome | classifier 的输出结构，含 ErrorClass + retryable + cooldown + rate_limit_hint + oauth_signal + passthrough_body |
| RequiredCapabilities | § 2.3 capability 集合 + 匹配模式 + provenance 链 |
| MappedModel | § 2.5 user_alias → upstream_model 映射结果 |
| AdapterCapabilityHook | § 2.7 adapter 注入的能力检测钩子 |
| BlacklistFailMode | § 3.2 黑名单 Redis down 时的策略 |
| UpstreamStreamPolicy | § 4.1 sync ↔ stream 桥接四态 |
| GeminiFileRef | § 4.4 gemini files API 缓存项 |
| TunnelResolution | § 5.5 选 tunnel 节点的输出 |
| LogVerbosity | § 6.4 detailed log 的三档 verbosity |
| KEK dual-read | § 1.7 KEK 轮换中间态 |

