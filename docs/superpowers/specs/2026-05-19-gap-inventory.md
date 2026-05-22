# 功能缺口清单（v2026-05-19）

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft（仅记录，未排期、未实施）
- **作者**：协作整理（Claude + 用户）
- **来源**：对照 `Aether/` 全栈实现（Python `src/`、aether-hub、aether-proxy）枚举 Rust 网关侧缺少的能力
- **范围**：网关数据面 + 与上游对接 + 请求生命周期 + Rust 自持状态。**排除**纯 Web 控制台（订阅 / 支付 / license / 邮件 / LDAP / 公告 / 审计 / 用户管理）——这些归 Go。
- **不在范围**：实施排期；P0/P1/P2 是粗粒度评估，不是承诺
- **关联**：
  - 已有 spec：[2026-05-19-adapter-layer-design.md](./2026-05-19-adapter-layer-design.md)、[2026-05-19-task-worker-design.md](./2026-05-19-task-worker-design.md)
  - 已有 plan：[2026-05-19-business-split-refactor.md](../plans/2026-05-19-business-split-refactor.md)
  - 主设计：[2026-05-18-llm-gateway-design.md](./2026-05-18-llm-gateway-design.md)

> 本文是 backlog，不是 spec；当条目被立项写成 spec 后，回头在条目末尾标注引用。

---

## 桶 1. 协议 / Codec / IR（P0）

> **spec 化进展（v2 重写）**：本桶已拆为 4 份 spec，**核心形态升级**：IR 从 runtime 数据通路降级为**纯编译期 schema**；codegen 生成 N² 个 wire→wire 直转；runtime 永不实例化 IR/schema。
> - [S1 IR + Codegen 基础](./2026-05-19-codec-s1-ir-and-codegen.md) v2（编译期 schema + 矩阵宏 + 语义 trait 派生 + WireAny enum）
> - [S2 Wire 多模态](./2026-05-19-codec-s2-wire-multimodal.md) v2（wire 形状定义 + `#[ir(...)]` 注解 + 跨 wire `cross::to_X` 特化 via + 动作型 trait per-wire 手写）
> - [S3 SSE 多事件](./2026-05-19-codec-s3-sse-multi-event.md) v2（流式 N² 直转 + 跨 chunk 累积状态 codegen + `parse_stream_chunk_to<B: WireStreamTarget>`）
> - [S4 响应侧 + 跨协议](./2026-05-19-codec-s4-response-and-cross-protocol.md) v2（响应 N² 直转 + `extract_usage_any` POD + 撤回 nonstream 400 短路）
> 依赖顺序：S1 → (S2 / S3 并行) → S4。

| # | 能力 | Aether 来源 | 落点 / 备注 |
|---|---|---|---|
| C1 | 多模态 ContentPart 完整 round-trip（image / audio / file URL+base64） | `conversion/image_resolver.py`、`internal_video.py` | 请求侧 → S2；响应侧 → S4 |
| C2 | tools / tool_choice / tool_use / tool_result 跨协议（OpenAI ↔ Anthropic blocks ↔ Gemini functionCall） | `conversion/internal.py`、`field_mappings.py` | 请求侧 → S2；响应侧 → S4 |
| C3 | thinking / reasoning 通道 + thinking 错触发整流 | `conversion/thinking_cache.py`、`services/message/thinking_rectifier.py` | IR 改动 → S1 (ADR-S1-1)；wire 请求侧 → S2；响应侧 → S4；signature 整流钩子 → gw-proxy 错误处理（桶 7 E6） |
| C4 | 跨协议**响应**体重渲染（非流） | `conversion/registry.py` | → S4（全量对称） |
| C5 | 多事件 SSE 缓冲（一个 upstream chunk → 多个 IR event） | `conversion/stream_state.py`、`stream_events.py` | → S3 |
| C6 | 同步 ↔ 流式桥接（客户端要 sync 但上游强制 stream，反之） | `services/provider/stream_policy.py:UpstreamStreamPolicy` | 待立项；依赖 S4 落地后再开始 |
| C7 | 内容编码处理（gzip / br / zstd 解压） | `core/http_compression.py`、`clients/http_client.py` | 待立项；放 gw-proxy |
| C8 | OpenAI Responses API 形状（distinct from chat/completions） | codex adapter 全用 | 待立项；codegen 兑现后只是「加 wire + 注解」 |
| C9 | Gemini Files API 中转 | `modules/gemini_files`、`services/gemini_files_mapping.py` | 待立项；上传/映射独立 |
| C10 | Codegen 兑现 `IrConvert` / `IrStreamConvert` derive 实际实现 | `gw-codec-macros` 当前空壳 | → S1（已是 S1 的核心内容） |

## 桶 2. Adapter / 上游差异化（P0）

> **spec 化进展**：本桶已拆为 8 份 spec（含 Kiro codec）：
> - [A-T0 Adapter trait v2](./2026-05-19-adapter-a-t0-trait-v2.md)（撤回 adapter-layer-design.md 的 BodyPatch；对齐 codec v2 WireAny）
> - [A-T1 claude_code](./2026-05-19-adapter-a-t1-claude-code.md)
> - [A-T2 gemini_cli](./2026-05-19-adapter-a-t2-gemini-cli.md)
> - [A-T3 vertex_ai](./2026-05-19-adapter-a-t3-vertex-ai.md)
> - [A-T4 antigravity](./2026-05-19-adapter-a-t4-antigravity.md)
> - [A-T5 codex](./2026-05-19-adapter-a-t5-codex.md)
> - [A-T6 kiro vendor shell](./2026-05-19-adapter-a-t6-kiro-vendor.md)
> - [A-T7 / Codec S5 kiro codec](./2026-05-19-codec-s5-kiro-eventstream.md)（归桶 1 codec 矩阵扩展，实际工作量在此）
> 依赖顺序：A-T0 → (A-T1/A-T2/A-T3/A-T4 并行) → A-T5 → A-T7 → A-T6。

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| A1 | adapter trait + 注册表 | adapter spec ADR-A1 | → A-T0（trait v2 升级，撤回原 ADR-A2 BodyPatch） |
| A2 | 快路径单字段 patch 通道（语义保持） | adapter spec ADR-A2 | **撤回**——codec v2 后所有 wire mutation 走 WireAny + 语义 trait，字节级 patch 不再需要 |
| A3 | claude_code adapter | `adapters/claude_code/*` | → A-T1 |
| A4 | gemini_cli adapter | `adapters/gemini_cli/*` | → A-T2 |
| A5 | vertex_ai adapter | `adapters/vertex_ai/*` | → A-T3 |
| A6 | antigravity adapter | `adapters/antigravity/*` | → A-T4 |
| A7 | codex adapter | `adapters/codex/*` | → A-T5 |
| A8 | kiro adapter | `adapters/kiro/*` | → A-T6 + A-T7（codec） |
| A9 | provider_ops（SaaS 余额查询/签到） | `services/provider_ops/` × 9 | **归 Go**——纯管理面定期任务，与 LLM 调用无关 |
| A10 | provider behavior + envelope + body_transformer | `provider/behavior.py`、`envelope.py` | → A-T0 trait v2 已涵盖 |
| A11 | upstream_headers / preset_models / format 注册 | `provider/upstream_headers.py` | vendor entity 配置驱动；Go 写，Rust 读 |
| A12 | 浏览器指纹（curl_cffi impersonate） | `provider/fingerprint.py` | → A-T0 trait `transport_profile` 字段已声明；HTTP client impl 独立 spec |
| A13 | prompt cache key 合成 | `provider/prompt_cache.py` | 各 vendor adapter 内 patch_request 实现 |

## 桶 3. 上游账号 / Key 状态机（P0-P1）

> **spec 化进展**：本桶已拆为 4 份 spec：
> - [K-A KeyHealth 多维状态机](./2026-05-19-key-health-k-a-state-machine.md)（新建 `gw-health` 业务 crate；by_api_format 分桶 + HTTP 码分级冷却 + `is_available(key, fmt)` 接口；纯进程内）
> - [K-B Vendor 错误响应分析钩子](./2026-05-19-key-health-k-b-vendor-error-hook.md)（`gw-adapter::error_analyzer` trait；HTTP 码 + 响应体关键词 → CooldownDirective + OAuthSignal）
> - [K-C OAuth Token 缓存 + 刷新](./2026-05-19-key-health-k-c-oauth-token.md)（`gw-adapter::oauth` 模块；懒刷新 + CAS lock；通过 K-A 暴露不可用态）
> - [K-D Quota Probe + Usage Snapshot](./2026-05-19-key-health-k-d-quota-probe.md)（被动 snapshot 解析器 + 主动 probe scheduler；写回 `entity:vendor_key.quota_*`）
> 依赖顺序：K-A → (K-B / K-C 并行) → K-D。

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| K1 | OAuth token 缓存 + 懒刷新 + 失败标记 | `provider/oauth_token.py` | → K-C |
| K2 | 账号级 block 状态前缀（`ACCOUNT_BLOCK_` / `OAUTH_EXPIRED_` / `OAUTH_REFRESH_FAILED_`） | 同上 | → K-C（K-A 的 CooldownReason enum 取代字符串前缀） |
| K3 | Key cooldown 状态机按 HTTP 码分级（401 / 402 / 403 / 429 / 529 / 400-disabled） | `pool/health_policy.py` | 状态机 → K-A；映射决策 → K-B |
| K4 | 三态熔断（CLOSED / OPEN / HALF_OPEN）按 api_format 分桶 | `health/monitor.py` | → K-A |
| K5 | quota probe / 主动配额探测（codex） | `provider_keys/pool_quota_probe_scheduler.py`、`codex_realtime_quota.py` | → K-D（主动 probe） |
| K6 | upstream usage snapshot 回写（上游真实配额） | `usage/upstream_usage_snapshot.py` | → K-D（被动 snapshot） |
| K7 | account_state 解析与 deactivation 自动恢复 | `pool/account_state.py` | → K-A（cooldown TTL 自动过期 = 自动恢复） |
| K8 | unschedulable_rules（响应体关键词匹配 → 标记冷却） | `pool/health_policy.py` | → K-B |

## 桶 4. 调度 / 候选选取（P1）

> **spec 化进展**：本桶已拆为 6 份 spec：
> - [R-AB Routing core + multi-score](./2026-05-19-routing-r-ab-multi-score.md)（CandidateFilter 链 + ScoringDimension trait + 8 dimension + SelectorAny 返回 SmallVec）
> - [R-C LatencyTracker](./2026-05-19-routing-r-c-latency-tracker.md)（per-key TTFT sliding window）
> - [R-D AffinityManager](./2026-05-19-routing-r-d-affinity-manager.md)（api_key → vendor_key 亲和 + AffinityDimension）
> - [R-E Failover 执行层](./2026-05-19-routing-r-e-failover.md)（执行层消费 SmallVec 列表；撤销 retry.rs）
> - [R-F Stream probe](./2026-05-19-routing-r-f-stream-probe.md)（首 chunk 前 failover 安全网）
> - [R-G Sticky TTL fix](./2026-05-19-routing-r-g-sticky-fix.md)（moka 替换 DashMap）
> 依赖顺序：R-AB → (R-C / R-D / R-G 并行) → R-E → R-F。

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| S1 | multi-score 维度组合（替换互斥 enum） | `pool/strategies/multi_score.py` | → R-AB |
| S2 | preset dimension 集合 | `pool/dimensions/*` | → R-AB（v1 实现 8 个：cache_affinity / cost_first / latency_first / load_balance / health_first / quota_balanced / plan_tier / recent_refresh）+ R-D 的 affinity |
| S3 | mutex_group（同组维度互斥） | 同上 | → R-AB（distribution 组示例：cache_affinity / load_balance / affinity 互斥）|
| S4 | candidate_resolver / builder / sorter 管线 | `services/orchestration/*` | → R-AB（FilterChain 替代 builder + sorter；resolver = SelectorAny::pick） |
| S5 | concurrency_checker / restriction_checker / quota_skipper | `services/scheduling/*` | → R-AB filter 链；CapabilityMatch filter 与桶 10 配套 |
| S6 | failover 策略 + 跳过策略 + 重试模式 | `services/candidate/failover.py` | → R-E（撤销 gw-routing::retry，统一 FailoverExecutor）|
| S7 | request_dispatcher（异步派发） | `services/orchestration/request_dispatcher.py` | → R-E + R-F |
| S8 | affinity manager（api_key 分桶 + 失效） | `services/scheduling/affinity_manager.py` | → R-D |
| S9 | Sticky 策略 TTL 清扫 | Rust 现有问题 | → R-G |

## 桶 5. 限流 / 配额 / 并发（P1）

> **spec 化进展**：本桶已拆为 3 份 spec：
> - [RL-Core](./2026-05-19-ratelimit-rl-core.md)（per-user / per-vendor-key RPM + RateLimitKind 分类 + K-B ErrorOutcome 集成）
> - [RL-IP](./2026-05-19-ratelimit-rl-ip.md)（IP 限流 per-IP governor + IP allowlist + IpBlockMode 扩字段）
> - [RL-Adaptive](./2026-05-19-ratelimit-rl-adaptive.md)（AIMD + 置信度衰减；学到上游真 RPM 主动下调）
> 依赖顺序：RL-Core → (RL-IP / RL-Adaptive 并行)。

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| R1 | adaptive RPM（429 后自动下调，慢恢复） | `rate_limit/adaptive_rpm.py` | → RL-Adaptive |
| R2 | rate-limit detector（区分 429：RPM / TPM / Daily / Monthly / Concurrent） | `rate_limit/detector.py` | → RL-Core（RateLimitKind 枚举） |
| R3 | IP rate limiter（L7 限流，DoS 防护） | `rate_limit/ip_limiter.py` | → RL-IP |
| R4 | user 维度 RPM | `rate_limit/user_rpm_limiter.py` | Rust 已有 + RL-Core 加 per-key 维度 |
| R5 | 用户并发数追踪（active requests） | `usage/active_requests.py` | Rust 已有 `per_user_streams: AtomicUsize` |
| R6 | 周期 quota 重置 | `usage/quota_scheduler.py` | **归 Go**（cost-based quota 在 Go 端，base_micro 周期重置） |
| R7 | daily_quota_tokens 限制 | README v1 gap | **归 Go**（cost-based quota；Rust 不实现 daily token counter） |
| R8 | hard cap 单请求 token 上限 | Rust 现有 | Rust 现有 `gw-auth::hard_cap`，本桶不动 |
| R9 | global stream cap | Rust 现有 | Rust 现有 `gw-proxy::filters::early`，本桶不动 |
| R10 | IP allowlist | README v1 gap | → RL-IP |
| - | 预留槽位机制（VIP 用户保留并发） | `concurrency_manager.py` | **v2**，本桶不立项 |
| - | TPM 限速 | - | **v2**，本桶不立项（仅暴露 RateLimitKind::Tpm 给 RL-Adaptive 观察用） |

## 桶 6. 计费 / 用量（P0-P1）

> **spec 化进展（v2 范围调整）**：**用户决议把"用量 → cost 公式"全部归 Go 侧实现**。Rust 网关仅做用量记录 + freeze 估算保护。受影响：refactor plan 里规划的 `gw-billing` crate **撤销**；B1/B2/B3/B4/B5/B8/B11 全部归 Go，Rust 端不实现。桶 6 拆为 5 份 spec：
> - [C-α UsageRecord schema](./2026-05-19-billing-c-alpha-usage-record.md)（BillingRecord 改名 + 删 cost_micro + 加 UsageSnapshot 6 维 + MarkupChainSnapshot；stream 改名 `usage:records`）
> - [C-β EstimatePricing entity](./2026-05-19-billing-c-beta-estimate-pricing.md)（Rust 用的保守估算 entity；撤销 cost 计算；timeout 字段分离）
> - [C-γ 余额读写分工](./2026-05-19-billing-c-gamma-balance-split.md)（Go 唯一写 base/deducted；Rust 唯一写 frozen；events:counter 订阅）
> - [C-δ Usage Stream DLQ](./2026-05-19-billing-c-delta-stream-dlq.md)（flusher 工程加固；三层兜底）
> - [C-ε Go 侧契约](../../go-contracts/main.md) — 已吸收至 `docs/go-contracts/`（main + parts），原文件 `2026-05-19-billing-c-epsilon-go-contract.md` 已删除
> 依赖顺序：C-α + C-β 并行 → C-γ → C-δ。C-ε 是契约文档，与实施并行。

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| B1 | Pricing 多维定价（input / output / cache_creation / cache_read / thinking / audio / image） | `billing/templates.py`、`models.py` | **归 Go**；Rust 仅记 UsageSnapshot 6 维（C-α） |
| B2 | 阶梯计价 + 模板（claude / openai / doubao 等） | `billing/templates.py`、`calculator.py` | **归 Go** |
| B3 | 安全表达式引擎（AST 白名单 + Decimal） | `billing/formula_engine.py` | **归 Go** |
| B4 | dimension_collector（动态维度采集） | `billing/dimension_collector_service.py` | **归 Go** |
| B5 | historical_rebilling（价改回溯） | `usage/historical_rebilling.py` | **归 Go** |
| B6 | usage events 多消费组 + DLQ | `usage/consumer_streams.py`；Rust README 列为 gap | → C-δ |
| B7 | telemetry_writer 分桶遥测 | `usage/telemetry.py`、`telemetry_writer.py` | UsageRecord 完整字段已覆盖（C-α）；分桶聚合归 Go |
| B8 | cache_analysis（自动推荐 TTL） | `usage/cache_analysis.py` | **归 Go**（离线分析） |
| B9 | token_normalization + usage_mapper（各 provider 字段名归一） | `billing/token_normalization.py`、`usage_mapper.py` | 桶 1 codec S4 `extract_usage_any` 已做 ✓ |
| B10 | lifecycle（请求级用量生命周期事件） | `usage/lifecycle.py`、`events.py` | 与 detailed_log 一起做，推后到独立 spec |
| B11 | model_markup 完整实现 + 用户级倍率链 | README 列为 gap | Rust 记 MarkupChainSnapshot 快照（C-α），公式应用归 Go |
| B12 | freeze 估算（已有；补三层 base/deducted/frozen） | Rust 现有 | → C-β + C-γ（Rust 仅 freeze；Go 写 deducted） |

## 桶 7. 错误处理 / 错误透传（P1）

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| E1 | error_classifier（纯分类，无副作用） | `orchestration/error_classifier.py` | gw-core::error 扩展 |
| E2 | error_handler（副作用：缓存失效、健康记录、RPM 调整、OAuth 标记） | `orchestration/error_handler.py` | gw-proxy + gw-routing + gw-adapter 共同侦听 |
| E3 | error_passthrough（上游真实错误体到客户端协议适配） | `provider/error_passthrough.py` | gw-codec |
| E4 | provider-specific error_enhancer（kiro） | `adapters/kiro/error_enhancer.py` | gw-adapter::kiro |
| E5 | resilience 全局降级 + 用户友好错误 | `core/resilience.py` | gw-proxy |
| E6 | ThinkingSignatureException 错触发整流（配 C3） | `services/message/thinking_rectifier.py` | gw-proxy 重试钩子 |

## 桶 8. 重试 / 失败转移（P1）

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| RT1 | 跨 key / cross-vendor 失败转移（已有 SameKey / CrossKey / Mixed 配置；需接 candidate 管线） | Rust 现有 retry.rs + Aether failover | gw-routing::retry + gw-proxy |
| RT2 | 流式不重试（已有） | Rust 现有 | gw-routing::retry |
| RT3 | 上游 stream probe（探测可行性后再决定） | `services/task/core/exceptions.py:StreamProbeError`、`candidate/failover.py` | gw-proxy + gw-routing |
| RT4 | 重试预算（全局） | Rust 现有 RetryConfig | gw-routing::retry |

## 桶 9. 鉴权 / 身份（P1）

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| AU1 | JWT 完整校验（iss / aud / nbf；已列 gap） | README gap | gw-auth::jwt |
| AU2 | JWT 黑名单（撤销态） | `services/auth/jwt_blacklist.py` | gw-auth + Redis |
| AU3 | API key 完整校验 + force_capabilities + expires_at | `core/access_restrictions.py`；README gap | gw-auth::apikey |
| AU4 | HMAC 签名强制（已列 gap） | README gap | gw-auth::signature |
| AU5 | management_tokens（管理 token，独立通道） | `modules/management_tokens` | gw-auth |
| AU6 | 五级权限合并（已有 ADR-023） | Rust 现有 | gw-auth::merge |
| AU7 | model_permissions 正则匹配 + ReDoS 保护（100ms） | `core/model_permissions.py` | gw-auth |

> AU4（session / refresh_cookie）原属 Go 侧，已剔除。

## 桶 10. 能力 / 路由参数（P1）

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| CP1 | capability resolver（模型级 / Key 强制 / header X-Require-Capability / adapter 检测 / 重试升级） | `services/capability/resolver.py` | gw-auth + gw-adapter（adapter 检测钩子） |
| CP2 | exclusive vs compatible 匹配模式 | `core/key_capabilities.py` | gw-routing 选 key 过滤 |
| CP3 | model mapper（用户请求模型 → provider 实际模型） | `services/model/mapper.py` | gw-routing + gw-store entity 扩列 |
| CP4 | model_availability 系统级可用性 | `services/model/availability.py` | gw-routing |
| CP5 | upstream model fetcher + scheduler（定期拉上游 model list） | `services/model/upstream_fetcher.py`、`fetch_scheduler.py` | 后台 scheduler；可下沉 Go |
| CP6 | pricing_strategy（模型 → 价目动态绑定） | `services/model/pricing_strategy.py` | gw-billing + gw-store |

## 桶 11. 异步任务 / 长作业（P1）

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| T1 | 异步任务提交（image / video 长任务） | `services/task/submit/*` | gw-task（spec 已成稿） |
| T2 | 任务轮询 + 状态机 | `services/task/polling/*`、`core/lifecycle.py` | gw-task / gw-task-runtime |
| T3 | video 任务专门通道 + 计费 | `services/task/video/*` | gw-billing + gw-task-runtime |
| T4 | sync_execute / async_submit 双模 | `services/task/execute/*` | gw-task-runtime |
| T5 | 任务计费 outcome_builder | `services/task/submit/outcome_builder.py` | gw-task-runtime + gw-billing |
| T6 | 任务 webhook 回调（provider 主动推） | （未决问题 #4） | gw-proxy 收 webhook → 写 Redis |

> 桶 11 已有 spec [2026-05-19-task-worker-design.md](./2026-05-19-task-worker-design.md)。

## 桶 12. 出网 / 隧道节点（P1，视部署）

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| TN1 | tunnel client（墙内 Rust 网关 → aether-hub） | `aether-hub` + `aether-proxy` | 独立 binary `gw-tunnel-client` 或下沉 reqwest custom transport |
| TN2 | tunnel frame protocol（二进制帧 stream_id / msg_type） | `aether-hub/src/protocol.rs` | 同上 |
| TN3 | tunnel pool + heartbeat + 拥塞退避 | `aether-hub/src/hub.rs` | 同上 |
| TN4 | proxy_nodes 节点选择（按 provider / account / key 维度） | `modules/proxy_nodes` | gw-adapter::transport |
| TN5 | safe_dns（防 DNS 污染） | `aether-proxy/src/safe_dns.rs` | 独立 lib 或 reqwest resolver |
| TN6 | target_filter（端口 / host 白名单） | `aether-proxy/src/target_filter.rs` | 同上 |

## 桶 13. 缓存与同步（P1）

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| H1 | model_cache / provider_cache / user_cache 多层（Rust 已有 EntityStore） | `services/cache/*` | gw-store::entity_store 扩展 |
| H2 | cache invalidation（基于事件） | `services/cache/invalidation.py`、`sync.py` | gw-store::stream_consumer 扩展 |
| H3 | StreamConsumer 写时同步（已有；补 DLQ + 重启对齐） | Rust 现有 + 已记录 issue | gw-store + issue follow-up |
| H4 | warm path no cold loader 修复 | Rust 已记录 issue | gw-store（已有 v1.1 plan） |

## 桶 14. 可观测性 / 运维（P2）

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| O1 | Prometheus 指标（已有；补 codec / freeze / security / kms 6 类） | Rust 现有 | gw-gateway::observability |
| O2 | format conversion 指标（direction / source / target / status） | `conversion/registry.py` | gw-codec |
| O3 | 健康监控 endpoint + circuit 暴露 | `services/health/endpoint.py`、`monitor.py` | gw-gateway::health |
| O4 | Debug 模式（请求级 opt-in + phase 时间 + jemalloc） | Rust 现有 | gw-proxy::filters::early |
| O5 | detailed_log + simple_log + 写入时脱敏 | Rust 现有 | gw-ledger + gw-proxy |
| O6 | metrics status-class collapse 修复 | Rust 已记录 issue | gw-proxy 修 |
| O7 | error log noise on short-circuit 修复 | Rust 已记录 | gw-proxy 已修（commit d45fdaf） |
| O8 | XREADGROUP parse error 修复 | Rust 已记录 issue | gw-store（已有 v1.1 plan） |
| O9 | stats_aggregator / time_range 聚合 | `services/system/*`（部分可放 Go） | 多数下沉 Go |

## 桶 15. 安全（P1）

| # | 能力 | Aether 来源 | 落点 |
|---|---|---|---|
| SE1 | IP 黑白名单（allowlist 已 gap） | README | gw-auth::ip_filter（同 R10） |
| SE2 | HMAC 签名强制 | README | gw-auth::signature（同 AU4） |
| SE3 | KMS + AES-256-GCM + KEK 轮换（已有；补轮换路径） | Rust 现有 | gw-gateway::kms |
| SE4 | model_permissions ReDoS 防护 | `core/model_permissions.py` | gw-auth（同 AU7） |
| SE5 | JWT 黑名单 fail-closed / open 开关 | `services/auth/jwt_blacklist.py` | gw-auth（同 AU2） |

---

## 已成稿 spec 与 plan 索引

| 主题 | 文档 | 状态 |
|---|---|---|
| Adapter 层分层边界 | [2026-05-19-adapter-layer-design.md](./2026-05-19-adapter-layer-design.md) | Draft |
| 异步任务 worker | [2026-05-19-task-worker-design.md](./2026-05-19-task-worker-design.md) | Draft v3.3（含 ADR-T16 部署可分离性） |
| Codec S1：IR + Codegen 基础 | [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md) | Draft |
| Codec S2：Wire 多模态 | [2026-05-19-codec-s2-wire-multimodal.md](./2026-05-19-codec-s2-wire-multimodal.md) | Draft |
| Codec S3：SSE 多事件 | [2026-05-19-codec-s3-sse-multi-event.md](./2026-05-19-codec-s3-sse-multi-event.md) | Draft |
| Codec S4：响应侧 + 跨协议 | [2026-05-19-codec-s4-response-and-cross-protocol.md](./2026-05-19-codec-s4-response-and-cross-protocol.md) | Draft |
| KeyHealth K-A：多维状态机 | [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md) | Draft |
| KeyHealth K-B：vendor 错误分析钩子 | [2026-05-19-key-health-k-b-vendor-error-hook.md](./2026-05-19-key-health-k-b-vendor-error-hook.md) | Draft |
| KeyHealth K-C：OAuth Token 缓存 | [2026-05-19-key-health-k-c-oauth-token.md](./2026-05-19-key-health-k-c-oauth-token.md) | Draft |
| KeyHealth K-D：Quota Probe | [2026-05-19-key-health-k-d-quota-probe.md](./2026-05-19-key-health-k-d-quota-probe.md) | Draft |
| Billing C-α：UsageRecord schema | [2026-05-19-billing-c-alpha-usage-record.md](./2026-05-19-billing-c-alpha-usage-record.md) | Draft |
| Billing C-β：EstimatePricing entity | [2026-05-19-billing-c-beta-estimate-pricing.md](./2026-05-19-billing-c-beta-estimate-pricing.md) | Draft |
| Billing C-γ：余额读写分工 | [2026-05-19-billing-c-gamma-balance-split.md](./2026-05-19-billing-c-gamma-balance-split.md) | Draft |
| Billing C-δ：Stream DLQ | [2026-05-19-billing-c-delta-stream-dlq.md](./2026-05-19-billing-c-delta-stream-dlq.md) | Draft |
| Billing C-ε：Go 侧契约 | [docs/go-contracts/](../../go-contracts/main.md) | Absorbed（main + parts；按 Go 角色组织） |
| Adapter A-T0：trait v2 | [2026-05-19-adapter-a-t0-trait-v2.md](./2026-05-19-adapter-a-t0-trait-v2.md) | Draft |
| Adapter A-T1：claude_code | [2026-05-19-adapter-a-t1-claude-code.md](./2026-05-19-adapter-a-t1-claude-code.md) | Draft |
| Adapter A-T2：gemini_cli | [2026-05-19-adapter-a-t2-gemini-cli.md](./2026-05-19-adapter-a-t2-gemini-cli.md) | Draft |
| Adapter A-T3：vertex_ai | [2026-05-19-adapter-a-t3-vertex-ai.md](./2026-05-19-adapter-a-t3-vertex-ai.md) | Draft |
| Adapter A-T4：antigravity | [2026-05-19-adapter-a-t4-antigravity.md](./2026-05-19-adapter-a-t4-antigravity.md) | Draft |
| Adapter A-T5：codex | [2026-05-19-adapter-a-t5-codex.md](./2026-05-19-adapter-a-t5-codex.md) | Draft |
| Adapter A-T6：kiro vendor | [2026-05-19-adapter-a-t6-kiro-vendor.md](./2026-05-19-adapter-a-t6-kiro-vendor.md) | Draft |
| Codec S5：Kiro EventStream | [2026-05-19-codec-s5-kiro-eventstream.md](./2026-05-19-codec-s5-kiro-eventstream.md) | Draft |
| Routing R-AB：multi-score | [2026-05-19-routing-r-ab-multi-score.md](./2026-05-19-routing-r-ab-multi-score.md) | Draft |
| Routing R-C：LatencyTracker | [2026-05-19-routing-r-c-latency-tracker.md](./2026-05-19-routing-r-c-latency-tracker.md) | Draft |
| Routing R-D：AffinityManager | [2026-05-19-routing-r-d-affinity-manager.md](./2026-05-19-routing-r-d-affinity-manager.md) | Draft |
| Routing R-E：Failover | [2026-05-19-routing-r-e-failover.md](./2026-05-19-routing-r-e-failover.md) | Draft |
| Routing R-F：Stream probe | [2026-05-19-routing-r-f-stream-probe.md](./2026-05-19-routing-r-f-stream-probe.md) | Draft |
| Routing R-G：Sticky fix | [2026-05-19-routing-r-g-sticky-fix.md](./2026-05-19-routing-r-g-sticky-fix.md) | Draft |
| RateLimit RL-Core：per-user/key RPM + 429 分类 | [2026-05-19-ratelimit-rl-core.md](./2026-05-19-ratelimit-rl-core.md) | Draft |
| RateLimit RL-IP：IP 限流 + allowlist | [2026-05-19-ratelimit-rl-ip.md](./2026-05-19-ratelimit-rl-ip.md) | Draft |
| RateLimit RL-Adaptive：AIMD 自适应 | [2026-05-19-ratelimit-rl-adaptive.md](./2026-05-19-ratelimit-rl-adaptive.md) | Draft |
| 业务/支撑分离 refactor | [../plans/2026-05-19-business-split-refactor.md](../plans/2026-05-19-business-split-refactor.md) | Plan |
| Warm path 修复 | [../plans/2026-05-19-v1-warm-path-fix.md](../plans/2026-05-19-v1-warm-path-fix.md) | Plan |
| v1.1 残余问题 | [../plans/2026-05-19-v1.1-residual-issues.md](../plans/2026-05-19-v1.1-residual-issues.md) | Plan |

---

## 优先级分桶（粗粒度，仅供后续讨论）

**P0（不补则主流量跑不通）**：C1 / C2 / C3 / C4 / C5 / C10 / A1-A4 / A7 / A8 / B1 / B11

**P1（差异化竞争力 + 商业化）**：A5 / A6 / A9-A13 / K1-K8 / S1-S9 / R1-R10 / B2-B12 / E1-E6 / RT1-RT4 / AU1-AU7 / CP1-CP6 / T1-T6 / SE1-SE5

**P2（运维质量）**：H1-H4 / O1-O9 / B7 / B8

**部署相关（视场景）**：TN1-TN6（仅墙内场景需要）

---

> 本文不指定排期。每个桶按需立项成独立 spec / plan 后，回头在条目末尾标 → 引用 spec / plan。
