# K-B — Vendor 错误响应分析钩子（v2026-05-19）

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界，未含实现）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 3 K-B；覆盖 K3 + K8）
  - [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md)（**硬前置**：K-B 喂给 K-A 的接口）
  - [2026-05-19-adapter-layer-design.md](./2026-05-19-adapter-layer-design.md)（adapter trait 总览）
- **范围**：在 `gw-adapter` 上新增一个**错误响应分析 trait**，让每个 vendor adapter 自己决定"什么 HTTP 状态码 + 什么响应体 → 标记 key 冷却多久 / 标记 OAuth 失效 / 标记账号 block"。adapter 把决策结果以 `CooldownDirective` 形态喂给 `gw-health::KeyHealthStore::apply_cooldown`。
- **不在范围**：
  - 冷却字段的运行时存储（→ K-A，gw-health 提供）；
  - OAuth token 刷新本身（→ K-C）；
  - quota probe（→ K-D）。

> K-B 是 K-A 与 vendor adapter 之间的"翻译层"——把上游响应的 vendor-specific 语义翻译为 K-A 的通用 cooldown directive。

---

## 1. 背景

K-A 决议：`gw-health::record(Failure {http_status})` 仅做 sliding window 累计，**不做 HTTP 码 → cooldown 的映射**。映射逻辑放 K-B 是因为：

- 401 在 OpenAI = `account_deactivated` 文案 → 长冷却；在 Anthropic 可能是 token 临时失效 → OAuth 刷新即可；
- 403 在 vendor A 是 "subscription inactive"（长冷却），在 vendor B 是 "rate limited by enterprise plan"（短冷却 + retry-after）；
- 400 体内含 `organization has been disabled` 才是账号死，体内含 `invalid model` 不应当冷却 key（这是 client error）。

Aether `services/provider/pool/health_policy.py` 已经把这套关键词与 TTL 表写得完整；本 spec 把同款语义在 Rust 落地，但**按 vendor 拆分**——通用 enum + per-vendor pattern table。

## 2. 关键判断

### ADR-KB-1 `UpstreamErrorAnalyzer` trait

在 `gw-adapter` 内：

```rust
// crates/gw-adapter/src/error_analyzer.rs

pub trait UpstreamErrorAnalyzer: Send + Sync + 'static {
    /// 名称，例如 "claude_code" / "gemini_cli" / "vertex_ai" / "noop"。
    fn name(&self) -> &'static str;

    /// 分析单次上游错误响应，给出冷却决策 + OAuth 信号（如需要）。
    /// 输入纯只读，不持有 wire struct（避免与 codec spec 耦合）。
    fn analyze(&self, ctx: &ErrorAnalyzeCtx<'_>) -> ErrorOutcome;
}

pub struct ErrorAnalyzeCtx<'a> {
    pub vendor_id: VendorId,
    pub key_id: KeyId,
    pub api_format: ApiFormat,
    pub http_status: u16,
    pub headers: &'a [(String, String)],    // 复用 gw-codec::Headers 别名
    pub body_preview: &'a [u8],              // 前 N KB，避免 GB 级响应体扫整体
    pub retry_after_secs: Option<u32>,       // headers 解析过的便捷字段
}

pub struct ErrorOutcome {
    /// 是否需要 K-A 应用 cooldown。None = 不冷却（client error 或瞬时不影响 key 健康）。
    pub cooldown: Option<CooldownDirective>,
    /// OAuth 子信号：是否需要 K-C 标记 token 失效 / 账号 block。
    pub oauth_signal: Option<OAuthSignal>,
    /// 是否要把上游响应当作 key 健康的"失败样本"喂给 sliding window。
    /// 默认 true；client error (4xx 非账号问题) 应当 false。
    pub count_as_failure: bool,
}

pub enum OAuthSignal {
    /// token 过期（K-C 应触发刷新）
    TokenExpired,
    /// 刷新失败（K-C 应标记 [OAUTH_REFRESH_FAILED]）
    RefreshFailed,
    /// 账号级 block（K-C 应标记 [ACCOUNT_BLOCK]，刷新 token 也救不回来）
    AccountBlocked,
}
```

### ADR-KB-2 通用 default analyzer

每个 vendor adapter 必须实现 `UpstreamErrorAnalyzer`；提供 `DefaultErrorAnalyzer` 作为兜底（对应非 OAuth 上游：直接 OpenAI / Anthropic / Gemini API key）：

```rust
// gw-adapter::error_analyzer::default

pub struct DefaultErrorAnalyzer;

impl UpstreamErrorAnalyzer for DefaultErrorAnalyzer {
    fn analyze(&self, ctx: &ErrorAnalyzeCtx<'_>) -> ErrorOutcome {
        match ctx.http_status {
            401 => ErrorOutcome {
                // 标准 API key 401 = key 被禁用/吊销 → 长冷却
                cooldown: Some(CooldownDirective {
                    reason: CooldownReason::Status401AccountDeactivated,
                    ttl_secs: 3600,
                    reset_consecutive: false,
                }),
                oauth_signal: None,  // 非 OAuth 上游
                count_as_failure: true,
            },
            402 => ErrorOutcome {
                cooldown: Some(CooldownDirective {
                    reason: CooldownReason::Status402PaymentRequired,
                    ttl_secs: 3600,
                    reset_consecutive: false,
                }),
                oauth_signal: None,
                count_as_failure: true,
            },
            403 if Self::body_matches(ctx.body_preview, FORBIDDEN_ACCOUNT_PATTERNS) =>
                ErrorOutcome {
                    cooldown: Some(CooldownDirective {
                        reason: CooldownReason::Status403AccountSuspended,
                        ttl_secs: 3600,
                        reset_consecutive: false,
                    }),
                    oauth_signal: None,
                    count_as_failure: true,
                },
            403 => ErrorOutcome {
                cooldown: Some(CooldownDirective {
                    reason: CooldownReason::Status403Forbidden,
                    ttl_secs: 300,
                    reset_consecutive: false,
                }),
                oauth_signal: None,
                count_as_failure: true,
            },
            400 if Self::body_matches(ctx.body_preview, ACCOUNT_DISABLE_PATTERNS) =>
                ErrorOutcome {
                    cooldown: Some(CooldownDirective {
                        reason: CooldownReason::Status400OrganizationDisabled,
                        ttl_secs: 3600,
                        reset_consecutive: false,
                    }),
                    oauth_signal: None,
                    count_as_failure: true,
                },
            400 => ErrorOutcome {
                cooldown: None,          // 客户端请求错误，不冷却 key
                oauth_signal: None,
                count_as_failure: false, // 也不算 key 失败
            },
            429 => ErrorOutcome {
                cooldown: Some(CooldownDirective {
                    reason: CooldownReason::Status429RateLimit,
                    ttl_secs: ctx.retry_after_secs.unwrap_or(60),
                    reset_consecutive: false,
                }),
                oauth_signal: None,
                count_as_failure: true,
            },
            529 => ErrorOutcome {
                cooldown: Some(CooldownDirective {
                    reason: CooldownReason::Status529Overloaded,
                    ttl_secs: 30,
                    reset_consecutive: false,
                }),
                oauth_signal: None,
                count_as_failure: true,
            },
            408 | 500 | 502 | 503 | 504 => ErrorOutcome {
                cooldown: Some(CooldownDirective {
                    reason: CooldownReason::StatusTransient5xx,
                    ttl_secs: 30,
                    reset_consecutive: false,
                }),
                oauth_signal: None,
                count_as_failure: true,
            },
            _ => ErrorOutcome {
                cooldown: None,
                oauth_signal: None,
                count_as_failure: ctx.http_status >= 500,
            },
        }
    }
}

const ACCOUNT_DISABLE_PATTERNS: &[&[u8]] = &[
    b"organization has been disabled",
    b"organization_disabled",
    b"account has been disabled",
    b"account_disabled",
    b"account has been deactivated",
    b"account_deactivated",
    b"account deactivated",
];

const FORBIDDEN_ACCOUNT_PATTERNS: &[&[u8]] = &[
    b"account suspended",
    b"account banned",
    b"account deactivated",
    b"subscription inactive",
    b"suspended",
    b"banned",
    b"deactivated",
];
```

`Self::body_matches` 用 memmem (`memchr` crate)，前 4KB body_preview 内做关键词扫描。

### ADR-KB-3 Vendor-specific analyzer

每个 OAuth vendor adapter 提供自己的 `UpstreamErrorAnalyzer` 实现，覆写 default 行为。例：

```rust
// gw-adapter::claude_code::error_analyzer

pub struct ClaudeCodeErrorAnalyzer;

impl UpstreamErrorAnalyzer for ClaudeCodeErrorAnalyzer {
    fn analyze(&self, ctx: &ErrorAnalyzeCtx<'_>) -> ErrorOutcome {
        match ctx.http_status {
            401 => {
                // Claude Code OAuth token 失效 vs 账号被封是两件事，需要看 body
                let body = ctx.body_preview;
                if body.contains_pattern(b"account_disabled") || body.contains_pattern(b"deactivated") {
                    // 账号死了，1h 冷却 + AccountBlocked
                    return ErrorOutcome {
                        cooldown: Some(CooldownDirective {
                            reason: CooldownReason::AccountBlocked,
                            ttl_secs: 3600,
                            reset_consecutive: false,
                        }),
                        oauth_signal: Some(OAuthSignal::AccountBlocked),
                        count_as_failure: true,
                    };
                }
                // 默认情况：token 过期，触发 OAuth 刷新（不冷却 key）
                ErrorOutcome {
                    cooldown: None,  // 等 K-C 刷新；刷新失败时由 K-C 喂 RefreshFailed cooldown
                    oauth_signal: Some(OAuthSignal::TokenExpired),
                    count_as_failure: false,  // OAuth 流程内的 401 不算 key 失败
                }
            }
            // 其余状态码委托 default
            _ => DefaultErrorAnalyzer.analyze(ctx),
        }
    }
}
```

vendor adapter 通过 trait 组合 + 委托 reuse default 的 90% 行为。

### ADR-KB-4 错误回路集成

在 `gw-proxy::filters` 的错误处理路径上调用 analyzer：

```rust
// gw-proxy::filters::response（伪代码，错误响应处理时机）
async fn on_upstream_error(ctx: &mut ReqCtx, status: u16, headers: ..., body_preview: ...) {
    let analyzer = adapter_registry.error_analyzer_for(&ctx.vendor_id);
    let outcome = analyzer.analyze(&ErrorAnalyzeCtx {
        vendor_id: ctx.vendor_id.clone(),
        key_id: ctx.vendor_key_id.clone(),
        api_format: ctx.api_format,
        http_status: status,
        headers,
        body_preview,
        retry_after_secs: parse_retry_after(headers),
    });

    // 1. 喂 cooldown 给 K-A
    if let Some(d) = outcome.cooldown {
        ctx.health.apply_cooldown(&ctx.vendor_key_id, ctx.api_format, d);
    }
    // 2. 喂 OAuth signal 给 K-C
    if let Some(sig) = outcome.oauth_signal {
        ctx.oauth_cache.handle_signal(&ctx.vendor_key_id, sig);  // K-C 接口
    }
    // 3. sliding window 累计
    if outcome.count_as_failure {
        ctx.health.record(&ctx.vendor_key_id, ctx.api_format, OutcomeKind::Failure {
            http_status: status,
            retry_after_secs: outcome.cooldown.as_ref().and_then(|d| Some(d.ttl_secs)),
        });
    } else {
        // 不算 key 失败但仍记录上游观测（success 路径）
        ctx.health.record(&ctx.vendor_key_id, ctx.api_format, OutcomeKind::Success);
    }
}
```

**关键边界**：analyzer 不写任何 store——所有副作用（cooldown / OAuth / sliding window）由 proxy 路径写入对应 store。analyzer 是纯函数 + 配置（关键词表）。

### ADR-KB-5 注册表

```rust
// gw-adapter::registry

pub struct AdapterRegistry {
    error_analyzers: HashMap<VendorId, Arc<dyn UpstreamErrorAnalyzer>>,
    default_analyzer: Arc<DefaultErrorAnalyzer>,
}

impl AdapterRegistry {
    pub fn error_analyzer_for(&self, vendor: &VendorId) -> Arc<dyn UpstreamErrorAnalyzer> {
        self.error_analyzers
            .get(vendor)
            .cloned()
            .unwrap_or_else(|| self.default_analyzer.clone())
    }
}
```

注册时机：`gw-gateway` bootstrap 阶段，按 vendor 类型注入。

### ADR-KB-6 body_preview 长度

默认 4096 字节。够覆盖所有 vendor 错误响应（实测 Aether 上的最长 reason 在 1KB 以内）。可配置：`GW_HEALTH__ERROR_BODY_PREVIEW_BYTES`，最大 16KB（防止恶意巨大错误体）。

### ADR-KB-7 retry-after 头解析

`parse_retry_after(headers) -> Option<u32>`：
- 数字秒数：直接采用，clamp 到 [1, 3600]；
- HTTP-date：解析为相对秒（v1 可选——若实现复杂可推后）；
- 异常字符串：返回 None。

放 `gw-adapter::error_analyzer::common`，供所有 analyzer 复用。

### ADR-KB-8 ErrorOutcome 的可观测性

每次 `analyze` 调用 emit：

| Metric | Label | 说明 |
|---|---|---|
| `gw_adapter_error_analyze_total` | vendor, http_status, decision | counter；decision = "cooldown" / "oauth_only" / "ignored" |
| `gw_adapter_error_cooldown_apply_total` | vendor, reason, ttl_bucket | counter；ttl_bucket = "short" (<60s) / "medium" (<600s) / "long" (≥600s) |
| `gw_adapter_error_keyword_hit_total` | vendor, pattern | counter；关键词命中（用于 tuning pattern 列表） |

### ADR-KB-9 测试矩阵

每个 vendor analyzer 至少覆盖：

- 401 + 账号 deactivated 体 → AccountBlocked cooldown 1h；
- 401 + 普通 unauthorized 体 → TokenExpired OAuth signal（不冷却 key）；
- 400 + organization disabled 体 → 长冷却；
- 400 + invalid model 体 → 不冷却；
- 429 + retry-after: 120 → 120s cooldown；
- 503 → 30s 短冷却；
- 200（不应被 analyze 调用，但若调用应当 return None）。

## 3. 实施大纲

依赖：K-A land；adapter spec ADR-A1 落地（registry 基础设施）。

1. 新建 `gw-adapter::error_analyzer` 模块；
2. 实施 `DefaultErrorAnalyzer` + 关键词表（参 §ADR-KB-2）；
3. `AdapterRegistry::error_analyzer_for` 注册表入口；
4. 改造 `gw-proxy::filters` 错误回路按 §ADR-KB-4 接入；
5. 单测每个分支；
6. metric 接入 + dashboard 字段定义。

vendor-specific analyzer（claude_code / gemini_cli / vertex_ai 等）作为后续 plan 实施。本 spec 只交付 Default + trait + 接入。

## 4. 未决问题

1. **关键词扫描的性能**：4KB body × `memmem` 多 pattern 扫描，预算 < 5µs 应该没问题。但若 vendor pattern 列表膨胀（>50 个 pattern），可能要考虑 Aho-Corasick 或 regex set。倾向 v1 用简单 `memmem`，metric `gw_adapter_error_analyze_duration_ms` 守住。
2. **关键词表的热更新**：当前是 const 静态表。是否要做配置驱动（运行时 reload）？倾向 v1 不做；新增关键词走 PR + 重启。
3. **vendor 自定义 retry-after 头名称**：标准是 `Retry-After`，但某些 vendor 可能有 `X-RateLimit-Reset-After` 等变种。倾向各 vendor analyzer 内部自行 parse；common 只提供 `Retry-After` 标准解析。
4. **OAuthSignal::TokenExpired 与 cooldown=None 的边界**：401 触发 OAuth 刷新但不冷却 key，这期间该 key 仍 available—— routing 可能在刷新完成前再次选中它，又 401。这是 K-C 责任：K-C 在收到 TokenExpired 信号后**短暂 lock 该 key**（参 K-C spec），routing 通过 K-A 的 is_available 看到 lock 状态（K-C 喂 cooldown_reason = OAuthRefreshing 短 TTL）。本 spec 与 K-C 接口边界确认。
5. **`ErrorAnalyzeCtx` 是否应该传递完整解析过的 ErrorIR**：当前直接传 raw body_preview + http_status，让 analyzer 自己 deser。是否传 codec 已解析的 ErrorIR（`gw-codec::parse_error`）更干净？倾向不——analyzer 是 vendor-specific，可能要看 codec 未解析的字段（vendor 自定义 header），raw 给最大灵活性。
6. **多 vendor 共享 pattern 表**：OpenAI / Anthropic / Gemini 的 `account_disabled` 关键词大致重叠。是否抽出 common pattern set？倾向 v1 各 vendor 独立 pattern；若发现重复严重再抽。

---

> K-B 完成后，error 路径上有"vendor → cooldown 决策 → K-A apply" 的完整链路。K-C 在 OAuth 路径上做对称工作。
