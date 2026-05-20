# A-T1 — claude_code Adapter(v2026-05-19)

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-adapter-a-t0-trait-v2.md](./2026-05-19-adapter-a-t0-trait-v2.md)(**硬前置**:trait + AdapterCtx)
  - [2026-05-19-key-health-k-c-oauth-token.md](./2026-05-19-key-health-k-c-oauth-token.md)(OauthCache 提供 access_token)
  - Aether 来源:`src/services/provider/adapters/claude_code/`
- **范围**：Claude Code 上游(Anthropic API + Claude Code CLI OAuth)的 adapter 实现:
  - rewrite_auth:OAuth bearer token(从 K-C 拿)+ anthropic-version + anthropic-beta 头;
  - rewrite_transport:URL 与 Anthropic 官方 endpoint 一致(无改动,但记录在 spec);
  - patch_request:注入 CLI 伪装头(`X-Stainless-*` 模拟 anthropic-sdk 行为);可选 system prompt 注入(用户配置驱动);
  - patch_response:无;
  - transport_profile:无(走标准 reqwest)。
- **不在范围**：
  - OAuth token 缓存与刷新机制(→ K-C);
  - 错误响应分析(→ K-B);
  - claude_code 特有的 quota probe(→ K-D vendor probe 实现);
  - 客户端身份限制(client_restriction.py 的 UA 检测,这属于客户端入口鉴权,**Rust 网关不实现**——客户端能否调用 claude_code 由 ApiKey 权限决定,不靠 UA 嗅探)。

> A-T1 是最简单的 vendor adapter,主要工作是头部伪装 + OAuth bearer。

---

## 1. 背景

Claude Code 是 Anthropic 官方推出的 CLI 工具,它访问 Anthropic API 时使用 OAuth flow(不是 API key)。Claude Code adapter 在 Aether 里承担:

- 把 OAuth token 当 Bearer 注入(Aether `envelope.py`);
- 注入 anthropic-version / anthropic-beta(含 claude-code-20250219 / oauth-2025-04-20 等 beta 标识);
- 注入 X-Stainless-* 头模仿官方 SDK 指纹(防上游检测到第三方实现);
- 检测客户端 UA / X-App=cli 决定是否允许使用此 vendor(Aether `client_restriction.py`)。

**第 4 项 v1 不做**——客户端能用哪个 vendor 由 ApiKey 权限决定(gw-auth 五级合并),不靠 UA 嗅探。UA 嗅探在反作弊层面意义有限(可伪造),不值得 v1 复杂度。

## 2. 设计决策

### ADR-AT1-1 vendor 注册

```rust
// crates/gw-adapter/src/claude_code/mod.rs
pub mod constants;
pub mod adapter;

pub use adapter::ClaudeCodeAdapter;

// 在 Registry::build() 时:
//   registry.register(VendorId::from("claude_code"), Arc::new(ClaudeCodeAdapter::new(...)));
```

vendor_id 字符串约定 `"claude_code"`(与 Aether 同名)。EntityStore 加载 vendor 时识别该 id,在 Registry 注入对应 adapter。

### ADR-AT1-2 constants

```rust
// crates/gw-adapter/src/claude_code/constants.rs

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

pub const BETA_CLAUDE_CODE: &str = "claude-code-20250219";
pub const BETA_OAUTH: &str = "oauth-2025-04-20";
pub const BETA_INTERLEAVED_THINKING: &str = "interleaved-thinking-2025-05-14";
pub const BETA_CONTEXT_1M: &str = "context-1m-2025-08-07";

pub const REQUIRED_BETA_TOKENS: &[&str] = &[
    BETA_CLAUDE_CODE,
    BETA_OAUTH,
    BETA_INTERLEAVED_THINKING,
];

/// Stainless SDK 伪装头(模拟 anthropic-sdk-js 的客户端指纹)。
/// 跟 Aether 同步;Anthropic 推出新版 sdk 时需同步更新。
pub const STAINLESS_HEADERS: &[(&str, &str)] = &[
    ("X-Stainless-Lang", "js"),
    ("X-Stainless-Package-Version", "0.70.0"),
    ("X-Stainless-OS", "Linux"),
    ("X-Stainless-Arch", "arm64"),
    ("X-Stainless-Runtime", "node"),
    ("X-Stainless-Runtime-Version", "v24.13.0"),
    ("X-Stainless-Retry-Count", "0"),
    ("X-Stainless-Timeout", "600"),
    ("X-App", "cli"),
    ("Anthropic-Dangerous-Direct-Browser-Access", "true"),
];
```

这些值跟 Aether 完全一致——一旦改动需要全栈同步(Rust + Go 部署都要看到)。

### ADR-AT1-3 ClaudeCodeAdapter 实现

```rust
// crates/gw-adapter/src/claude_code/adapter.rs

pub struct ClaudeCodeAdapter {
    /// 提供 OAuth refresher 给 K-C 调用(adapter 持引用)
    refresher: Arc<dyn OAuthRefresher>,
}

#[async_trait]
impl UpstreamAdapter for ClaudeCodeAdapter {
    fn name(&self) -> &'static str { "claude_code" }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        // 1. 通过 K-C 拿当前 access_token(自动刷新)
        let token = ctx.oauth_cache.get_access_token(&ctx.vendor_key, &*self.refresher).await?;

        // 2. 设 Bearer 头(撤销 x-api-key,Anthropic OAuth 用 Bearer)
        ctx.headers_mut.remove("x-api-key");
        ctx.headers_mut.set("authorization", &format!("Bearer {}", token.expose_secret()));

        // 3. anthropic-version
        ctx.headers_mut.set("anthropic-version", constants::ANTHROPIC_VERSION);

        // 4. anthropic-beta(合并客户端已经有的 + REQUIRED_BETA_TOKENS)
        let existing = ctx.headers_mut.get("anthropic-beta").unwrap_or_default().to_owned();
        let merged = merge_beta_tokens(&existing, constants::REQUIRED_BETA_TOKENS);
        ctx.headers_mut.set("anthropic-beta", &merged);

        Ok(())
    }

    fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        // Anthropic 官方 endpoint,URL 由 vendor entity 配置驱动,本 adapter 不动。
        // 若以后要走"接 vendor entity 默认 host = https://api.anthropic.com"逻辑,在此处兜底。
        Ok(())
    }

    fn patch_request(&self, _ctx: &AdapterCtx<'_>, _wire: &mut WireAnyRequest<'_>)
        -> Result<(), AdapterError>
    {
        // Stainless headers 在 rewrite_transport 阶段已经能注;放这里也行,但 wire 改写不涉及。
        // claude_code 不注 system prompt(用户实际请求里已经带,Aether 也不强加)。
        Ok(())
    }

    // patch_response 默认 no-op
}

/// 合并 beta token 字符串:逗号分隔,去重(顺序保留)
fn merge_beta_tokens(existing: &str, required: &[&str]) -> String {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for tok in existing.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if seen.insert(tok.to_owned()) {
            out.push(tok.to_owned());
        }
    }
    for tok in required {
        if seen.insert(tok.to_string()) {
            out.push(tok.to_string());
        }
    }
    out.join(",")
}
```

注意:
- Stainless headers 注入位置,实测哪个钩子里方便都行。本 spec 选 `rewrite_auth` 末尾顺手做(因为 rewrite_auth 已经在写 headers);如 vendor 标准化让 rewrite_transport 才合理,迁移很简单。
- patch_request 留空——claude_code 客户端实际就是 anthropic 协议,wire 不需要改。

### ADR-AT1-4 OAuth refresher 实现

K-C 规定 adapter 持 `Arc<dyn OAuthRefresher>`。claude_code 的 refresher 实现:

```rust
// crates/gw-adapter/src/claude_code/refresher.rs

pub struct ClaudeCodeRefresher {
    http: reqwest::Client,
    /// Claude Code OAuth client_id;固定值,跟 Aether 同步
    client_id: String,
    /// token endpoint URL;通常 https://console.anthropic.com/v1/oauth/token
    token_url: String,
}

#[async_trait]
impl OAuthRefresher for ClaudeCodeRefresher {
    async fn refresh(&self, current_refresh_token: &SecretString)
        -> Result<RefreshedToken, RefreshError>
    {
        let body = json!({
            "grant_type": "refresh_token",
            "refresh_token": current_refresh_token.expose_secret(),
            "client_id": self.client_id,
        });

        let resp = self.http.post(&self.token_url)
            .header("anthropic-beta", constants::BETA_OAUTH)
            .json(&body)
            .send()
            .await
            .map_err(|e| RefreshError::Transient { detail: e.to_string() })?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        match status.as_u16() {
            200 => {
                let parsed: ClaudeRefreshResponse = serde_json::from_str(&text)
                    .map_err(|e| RefreshError::Permanent { detail: format!("decode: {e}") })?;
                Ok(RefreshedToken {
                    access_token: SecretString::new(parsed.access_token),
                    refresh_token: parsed.refresh_token.map(SecretString::new),
                    expires_in_secs: parsed.expires_in.unwrap_or(3600),
                })
            }
            400 | 401 => {
                // refresh_token 失效或被吊销
                // 通过 body 区分:invalid_grant = AccountBlocked, 其他 = Permanent
                if text.contains("invalid_grant") || text.contains("account") {
                    Err(RefreshError::AccountBlocked { detail: text })
                } else {
                    Err(RefreshError::Permanent { detail: text })
                }
            }
            429 | 500..=599 => {
                Err(RefreshError::Transient { detail: format!("{status}: {text}") })
            }
            _ => Err(RefreshError::Permanent { detail: format!("{status}: {text}") }),
        }
    }
}

#[derive(Deserialize)]
struct ClaudeRefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u32>,
}
```

K-C `OauthCache` 的 `get_access_token` 看到 `expires_at_ms < now + 60s` → 触发 refresh → 调本 refresher。

### ADR-AT1-5 K-B 错误分析挂钩

参 K-B,claude_code 的 ErrorAnalyzer:

```rust
// crates/gw-adapter/src/claude_code/error_analyzer.rs

pub struct ClaudeCodeErrorAnalyzer;

impl UpstreamErrorAnalyzer for ClaudeCodeErrorAnalyzer {
    fn name(&self) -> &'static str { "claude_code" }

    fn analyze(&self, ctx: &ErrorAnalyzeCtx<'_>) -> ErrorOutcome {
        match ctx.http_status {
            401 => {
                let body = ctx.body_preview;
                if body.contains_pattern(b"account_disabled") ||
                   body.contains_pattern(b"deactivated") ||
                   body.contains_pattern(b"banned")
                {
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
                // 普通 401:OAuth token 失效,触发 K-C 刷新,不冷却 key
                ErrorOutcome {
                    cooldown: None,
                    oauth_signal: Some(OAuthSignal::TokenExpired),
                    count_as_failure: false,
                }
            }
            // 委托 default
            _ => DefaultErrorAnalyzer.analyze(ctx),
        }
    }
}
```

注册时与 adapter 一起进 Registry:

```rust
registry.register_analyzer(VendorId::from("claude_code"), Arc::new(ClaudeCodeErrorAnalyzer));
```

### ADR-AT1-6 配置(Vendor entity)

`Vendor` entity(`gw-core::entity::Vendor`)需要补两个字段供 claude_code 用:

```
Vendor {
    // 现有字段
    // ...
    /// vendor 默认上游 host(如 "api.anthropic.com")
    pub default_host: String,
    /// vendor 默认 path 前缀(如 "/v1")
    pub default_path_prefix: String,
}
```

这俩字段属于通用 vendor 配置,不只 claude_code 用——但 v1 之前没用上,本 spec 顺手提议补。Go 管理面写入,Rust 通过 entity 加载。

### ADR-AT1-7 测试矩阵

单测:
- rewrite_auth 注入 Bearer + anthropic-version + anthropic-beta 含所有 required beta;
- merge_beta_tokens 在客户端已经有 `claude-code-20250219, custom-beta` 时不重复添加;
- rewrite_auth 撤销已有的 x-api-key 头(防止 OAuth + api-key 共存导致上游拒);
- refresher 成功 → RefreshedToken;
- refresher 400 invalid_grant → AccountBlocked;
- error_analyzer 401 + account_disabled → AccountBlocked cooldown 1h;
- error_analyzer 401 普通 → TokenExpired signal,不冷却。

集成测(可选,需 mock Anthropic OAuth endpoint):
- 第一次请求触发 refresher;
- 第二次请求命中缓存(< 1ms);
- expires_at 临近触发后台 refresh 不阻塞当前请求。

## 3. 实施大纲

依赖:A-T0 trait v2 land;K-A、K-B、K-C 全 land。

1. 新建 `crates/gw-adapter/src/claude_code/` 模块;
2. constants.rs(beta token + Stainless header 常量);
3. adapter.rs(`ClaudeCodeAdapter` + `merge_beta_tokens`);
4. refresher.rs(`ClaudeCodeRefresher` + `ClaudeRefreshResponse`);
5. error_analyzer.rs(`ClaudeCodeErrorAnalyzer`);
6. mod.rs 导出 + `Registry::build_default()` 注册;
7. 单测覆盖所有分支;
8. 接入 `gw-proxy::filters::upstream` 经 Registry 调用。

## 4. 未决问题

1. **Stainless headers 版本同步**: 这些 fake 头模仿的是 Anthropic 官方 SDK 某个版本。SDK 升级后(如 v0.71)Anthropic 可能调整指纹算法。倾向定期(每月)check Aether 该文件是否更新,Rust 同步;长期看应当从 entity 配置驱动而非 const,但 v1 const 够用。
2. **client_id 来源**: Claude Code OAuth 用的 client_id 是 Anthropic 在 CLI 工具源码里写死的。该值是公开还是机密?倾向当机密处理,由 env(`GW_ADAPTER__CLAUDE_CODE__CLIENT_ID`)注入 ClaudeCodeRefresher,不写代码。
3. **token_url 配置**: 同上,放 env 或 vendor entity。倾向 vendor entity(每 vendor 独立,管理面 UI 可改)。
4. **OAuth flow 初始授权**: 第一次取 refresh_token 怎么来?这是用户在 Claude Code CLI 里登录 → 把 refresh_token 录到 Go 管理面 → 写 VendorKey entity。adapter 只负责后续刷新,**不**实现初始授权 flow——那是 Go 管理面职责。
5. **interleaved-thinking-2025-05-14 beta 是否常驻**: thinking 块的支持依赖该 beta。若客户端没用 thinking 但我们注了 beta,可能让上游开启 thinking 计费维度(用户不一定想要)。倾向 v1 始终注(Aether 行为),后续按客户端请求是否含 thinking_config 条件注。
6. **context-1m beta 是否常驻**: 1M context 是付费上下文,默认开启可能让用户意外花钱。倾向 v1 **不**注 BETA_CONTEXT_1M(列在常量里但不在 REQUIRED_BETA_TOKENS),用户显式要 1M 上下文时在请求 anthropic-beta 头上自己加。
7. **claude_code adapter 是否要做 quota probe**: 参 K-D。Claude Code OAuth 账号有"每月用量"概念。但 Anthropic 没有官方 quota dashboard API——只能从 429 retry-after 反推。倾向 v1 不做 active probe(passive snapshot 即可);若发现需要,K-D 独立加 ClaudeCodeQuotaProber。

---

> A-T1 是 6 个 vendor adapter 里最轻量的(主要是头部伪装 + OAuth)。A-T2/A-T3 复杂度类似;A-T5(codex)涉及 patch_request body 改写;A-T6/A-T7(kiro)涉及新 codec。
