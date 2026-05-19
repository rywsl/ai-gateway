# A-T2 — gemini_cli Adapter(v2026-05-19)

- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-adapter-a-t0-trait-v2.md](./2026-05-19-adapter-a-t0-trait-v2.md)(**硬前置**:trait + AdapterCtx)
  - [2026-05-19-key-health-k-c-oauth-token.md](./2026-05-19-key-health-k-c-oauth-token.md)(OauthCache)
  - [2026-05-19-key-health-k-d-quota-probe.md](./2026-05-19-key-health-k-d-quota-probe.md)(K-D 处理 quota 字段)
  - Aether 来源:`src/services/provider/adapters/gemini_cli/`
- **范围**：Gemini CLI 上游(Google Code Assist API + Gemini CLI OAuth flow)的 adapter 实现:
  - rewrite_auth:OAuth bearer + User-Agent 伪装(模拟 gemini-cli);
  - rewrite_transport:URL 改写到 v1internal(`POST /v1internal:streamGenerateContent` 等)+ project_id 注入 URL;
  - patch_request:wire 上 cloudaicompanionProject 字段注入 + ideType / platform 元数据(包在 request body 的特定字段里);
  - patch_response:无;
  - transport_profile:无。
- **不在范围**：
  - OAuth refresh(→ K-C);
  - quota header 提取(→ K-D 的 `GeminiCliQuotaExtractor`);
  - loadCodeAssist / onboardUser 这些初始化 flow(由 Go 管理面在录入 vendor key 时跑一次,Rust 不参与)。

> A-T2 与 A-T1 复杂度同级。主要差异:Gemini Code Assist 用一个非标准的 `v1internal` API 路径与请求 envelope;adapter 需要在 wire 上注入 project_id 与 client metadata。

---

## 1. 背景

Gemini CLI(`@google/generative-ai-cli`)使用 Google Code Assist 后端,跟 Gemini API 是两套不同 endpoint:

- 标准 Gemini:`https://generativelanguage.googleapis.com/v1/models/<m>:generateContent`,鉴权用 API key;
- Code Assist:`https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent`,鉴权用 OAuth bearer + `cloudaicompanionProject` 字段。

Aether `gemini_cli/envelope.py`(没贴出但实际工作)做的事是:把标准 Gemini 请求 envelope 重写为 Code Assist envelope,在 body 顶层加 `project` / `clientMetadata`(`ideType: ANTIGRAVITY` / `platform: PLATFORM_UNSPECIFIED` / `pluginType: GEMINI`)。

## 2. 设计决策

### ADR-AT2-1 vendor 注册

```
vendor_id = "gemini_cli"
```

### ADR-AT2-2 constants

```rust
// crates/gw-adapter/src/gemini_cli/constants.rs

pub const PROD_BASE_URL: &str = "https://cloudcode-pa.googleapis.com";

/// v1internal action path 模板,gemini_cli 用的 action:
///   - "streamGenerateContent" / "generateContent" / "countTokens" /
///     "loadCodeAssist" / "onboardUser"
pub fn v1internal_path(action: &str) -> String {
    format!("/v1internal:{}", action)
}

/// 上游必需头(模仿 gemini-cli)
pub const ACCEPT_ENCODING: &str = "identity";
pub const DEFAULT_USER_AGENT: &str = "gemini-cli/0.1.0 (linux; x64) Node.js/v22";

/// Code Assist 必需的 client metadata。
pub const CLIENT_METADATA: ClientMetadata = ClientMetadata {
    ide_type: "ANTIGRAVITY",
    platform: "PLATFORM_UNSPECIFIED",
    plugin_type: "GEMINI",
};

#[derive(Clone, Copy)]
pub struct ClientMetadata {
    pub ide_type: &'static str,
    pub platform: &'static str,
    pub plugin_type: &'static str,
}
```

### ADR-AT2-3 GeminiCliAdapter 实现

```rust
// crates/gw-adapter/src/gemini_cli/adapter.rs

pub struct GeminiCliAdapter {
    refresher: Arc<dyn OAuthRefresher>,
}

#[async_trait]
impl UpstreamAdapter for GeminiCliAdapter {
    fn name(&self) -> &'static str { "gemini_cli" }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let token = ctx.oauth_cache.get_access_token(&ctx.vendor_key, &*self.refresher).await?;
        ctx.headers_mut.remove("x-goog-api-key");
        ctx.headers_mut.remove("x-api-key");
        ctx.headers_mut.set("authorization", &format!("Bearer {}", token.expose_secret()));
        ctx.headers_mut.set("user-agent", constants::DEFAULT_USER_AGENT);
        ctx.headers_mut.set("accept-encoding", constants::ACCEPT_ENCODING);
        Ok(())
    }

    fn rewrite_transport(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        // 客户端请求路径如:/v1beta/models/gemini-2.0-flash:generateContent
        // 改写为:/v1internal:generateContent(或 streamGenerateContent,看 stream 标志)
        let action = match (ctx.upstream_protocol, is_stream_request(ctx)) {
            (ProtocolKind::Gemini, true) => "streamGenerateContent",
            (ProtocolKind::Gemini, false) => "generateContent",
            _ => return Err(AdapterError::PatchRequest(
                "gemini_cli only supports Gemini protocol upstream".into()
            )),
        };
        ctx.url_mut.set_host(host_without_scheme(constants::PROD_BASE_URL));
        ctx.url_mut.set_scheme("https");
        ctx.url_mut.set_path(&constants::v1internal_path(action));
        Ok(())
    }

    fn patch_request(&self, ctx: &AdapterCtx<'_>, wire: &mut WireAnyRequest<'_>)
        -> Result<(), AdapterError>
    {
        // gemini_cli 要求 body 里加 `project` / `request` / `model` envelope。
        // 这里用 codec 提供的 helper:`HasGeminiCliEnvelope::wrap_for_code_assist`。
        // ※ 该 helper 需要 codec S2 在 GeminiRequest 上加一个动作型 trait(GeminiCliEnvelope)。
        let project_id = extract_project_id(&ctx.vendor_key)
            .ok_or(AdapterError::MissingConfig("gemini_cli vendor key missing project_id"))?;
        wire.wrap_for_code_assist_any(
            &project_id,
            &ctx.model.0,
            &constants::CLIENT_METADATA,
        ).map_err(|e| AdapterError::PatchRequest(e.to_string()))?;
        Ok(())
    }
}

fn is_stream_request(ctx: &AdapterCtx<'_>) -> bool {
    // 直接从 ctx 上的标志位读;codec v2 已经在 stream 字段标过
    // 实施细节:gw-proxy 在装 ctx 时把 wire.is_stream() 拷过来
    ctx.is_stream
}

fn extract_project_id(key: &VendorKey) -> Option<String> {
    // vendor_key 上的额外字段:project_id 存在 VendorKey::tags 或专用 field
    // 实施:vendor_key 字段表扩列(Vendor entity 加 project_id 字段或走 metadata)
    key.metadata.get("gemini_cli.project_id").cloned()
}
```

### ADR-AT2-4 wire envelope wrapping(关键)

Code Assist 的请求 body 形态:

```json
{
  "model": "models/gemini-2.0-flash",
  "project": "projects/<project_id>",
  "request": {
    "contents": [...],
    "systemInstruction": {...},
    "generationConfig": {...},
    "tools": [...],
    "toolConfig": {...},
    "safetySettings": [...]
  },
  "clientMetadata": {
    "ideType": "ANTIGRAVITY",
    "platform": "PLATFORM_UNSPECIFIED",
    "pluginType": "GEMINI"
  }
}
```

标准 Gemini 请求是把 `contents / generationConfig / ...` 直接放顶层。adapter 需要"打包"成 Code Assist envelope。

实施:codec S2 已经为 GeminiRequest 实现了语义 trait;再加一个**动作型 trait**给 gemini_cli 专用:

```rust
// crates/gw-codec/src/gemini/ops.rs
pub trait HasGeminiCliEnvelope {
    fn wrap_for_code_assist(&mut self, project_id: &str, model: &str, meta: &ClientMetadata)
        -> Result<(), CodecError>;
}

impl HasGeminiCliEnvelope for GeminiRequest<'_> {
    fn wrap_for_code_assist(&mut self, project_id: &str, model: &str, meta: &ClientMetadata) -> ... {
        // 内部把 self 字段移到一个临时 envelope struct,然后通过 serde 重写为新形态。
        // 或者:用一个新的 wire 类型 GeminiCliRequest,wrap 时类型变化。
        // 后者更干净但需要 wire 矩阵新增一个 wire——倾向用 #[ir(skip)] 字段动态承载。
        ...
    }
}
```

⚠️ **关键决策**:wire envelope wrapping 是否要新增一个独立 wire 类型(GeminiCliRequest)进入 codec 矩阵?

**选项 A**: 新增 `GeminiCliRequest` wire 进矩阵——N² 直转加一档。语义清晰,但 N 从 3 升到 4(请求矩阵 16 个直转)。
**选项 B**: 复用 `GeminiRequest`,在 ops.rs 里实现 wrap 把 inner 数据搬到一个临时容器,序列化时 envelope wrap。语义模糊,但矩阵不动。

倾向 A——envelope wrap 不是临时操作,它是个真实的 wire 形态。这条放未决问题让用户拍。

### ADR-AT2-5 OAuth refresher

类似 A-T1,只是 token endpoint 不同。Google OAuth refresh:

```rust
pub struct GeminiCliRefresher {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,  // Google OAuth 需要 secret(可从 gemini-cli 源码取)
}

#[async_trait]
impl OAuthRefresher for GeminiCliRefresher {
    async fn refresh(&self, current_refresh_token: &SecretString)
        -> Result<RefreshedToken, RefreshError>
    {
        let resp = self.http
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", current_refresh_token.expose_secret().as_str()),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
            ])
            .send()
            .await
            .map_err(|e| RefreshError::Transient { detail: e.to_string() })?;

        // 错误分类同 A-T1:
        // 400 invalid_grant → AccountBlocked
        // 401 → 通常 invalid_client(配置问题)→ Permanent
        // 429/5xx → Transient
        // ...
    }
}
```

### ADR-AT2-6 K-B 错误分析

```rust
impl UpstreamErrorAnalyzer for GeminiCliErrorAnalyzer {
    fn analyze(&self, ctx: &ErrorAnalyzeCtx<'_>) -> ErrorOutcome {
        match ctx.http_status {
            // Gemini 把 quota exceeded 包成 403 PERMISSION_DENIED,而不是 429。
            // K-B body 关键词识别。
            403 => {
                let body = ctx.body_preview;
                if body.contains_pattern(b"PERMISSION_DENIED") &&
                   body.contains_pattern(b"quota")
                {
                    return ErrorOutcome {
                        cooldown: Some(CooldownDirective {
                            reason: CooldownReason::Status429RateLimit,
                            ttl_secs: 60,  // 短冷却,等 quota 自动重置
                            reset_consecutive: false,
                        }),
                        oauth_signal: None,
                        count_as_failure: true,
                    };
                }
                DefaultErrorAnalyzer.analyze(ctx)
            }
            // 401 同 A-T1,token 过期触发 K-C
            401 => ErrorOutcome {
                cooldown: None,
                oauth_signal: Some(OAuthSignal::TokenExpired),
                count_as_failure: false,
            },
            _ => DefaultErrorAnalyzer.analyze(ctx),
        }
    }
}
```

### ADR-AT2-7 K-D quota extractor

```rust
// crates/gw-adapter/src/gemini_cli/quota.rs
pub struct GeminiCliQuotaExtractor;

impl QuotaSnapshotExtractor for GeminiCliQuotaExtractor {
    fn vendor_id(&self) -> &VendorId { /* "gemini_cli" */ }

    fn extract(&self, _headers: &[(String, String)], body_preview: &[u8])
        -> Option<QuotaExtract>
    {
        // Gemini Code Assist 在响应 body 里返回 `quotaInfo` 字段(不是 header)
        // 解析 JSON 找 totalQuota / usedQuota / refreshAtMs
        let v: serde_json::Value = serde_json::from_slice(body_preview).ok()?;
        let quota = v.get("quotaInfo")?;
        let remaining = quota.get("remainingDailyTokens")?.as_u64()?;
        let window_secs = quota.get("nextResetAt").and_then(|n| n.as_str())
            .and_then(parse_rfc3339_to_secs_from_now);
        Some(QuotaExtract {
            remaining,
            unit: QuotaUnit::Tokens,
            window_secs,
        })
    }
}
```

K-D 的 prober(主动探测)用 loadCodeAssist endpoint:

```rust
impl QuotaProber for GeminiCliQuotaProber {
    async fn probe(&self, key: &VendorKey, token: &SecretString)
        -> Result<QuotaExtract, ProbeError>
    {
        // POST /v1internal:loadCodeAssist 拿到 currentTier 与 quota
        // 这是 Google 提供的"账号档位查询" endpoint。
        ...
    }
}
```

### ADR-AT2-8 测试

- rewrite_auth 注入 Bearer + UA + Accept-Encoding;
- rewrite_transport 把 `/v1beta/models/foo:generateContent` → `/v1internal:generateContent`;
- rewrite_transport stream 请求 → `:streamGenerateContent`;
- patch_request wrap_for_code_assist 后 body 含 `project` / `clientMetadata` 顶层字段;
- error_analyzer 403 PERMISSION_DENIED quota → 60s cooldown;
- refresher invalid_grant → AccountBlocked;
- quota_extractor 解析 quotaInfo 正确。

## 3. 实施大纲

依赖:A-T0 land + K-A/K-B/K-C/K-D land + codec S2 land(GeminiRequest 形态)。

1. `crates/gw-adapter/src/gemini_cli/` 模块;
2. constants.rs / adapter.rs / refresher.rs / error_analyzer.rs / quota.rs;
3. codec S2 配套:`HasGeminiCliEnvelope` 动作型 trait + GeminiRequest impl(若选 envelope wrap = ops 路径);
4. 或者 codec 矩阵增加 GeminiCliRequest wire(选项 A);
5. Vendor entity 字段补 `metadata.gemini_cli.project_id`(或新增专用字段);
6. 单测覆盖。

## 4. 未决问题

1. **envelope wrap 选项 A vs B**(§ADR-AT2-4): 是否新增 GeminiCliRequest 入 codec 矩阵?A 干净但 N=4;B 复用 GeminiRequest 但 wrap 时形态模糊。倾向 A;若 N² 增长过快可重新评估。
2. **project_id 来源**: 每个 vendor_key 一个 project_id。放 vendor_key.metadata 还是新增 VendorKey 字段?倾向用 metadata HashMap(避免 VendorKey 频繁加字段),约定 key 命名空间 `gemini_cli.project_id`。
3. **client_id / client_secret**: 同 A-T1 §4#2,env 注入;不写代码。
4. **loadCodeAssist / onboardUser 初始化**: 用户第一次绑定 vendor key 时,Aether 调用 loadCodeAssist 来拿 project_id 与 plan_type。这个流程归 Go 管理面,Rust 网关收到的 vendor_key 已经包含 project_id。
5. **plan_type 对计费的影响**: Aether `extract_plan_type` 区分 free/standard/legacy_paid/standard_with_paid_tier 等。Rust 网关现在不算 cost(归 Go),不需要 plan_type 路由决策——但 K-A 的 ApiFormat 维度可能要按 plan_type 分桶(同一 key 在 free tier 下 quota 紧,paid tier 下宽松)。倾向 v1 不分,后续根据 K-D quota probe 数据判断。
6. **stream 检测**: `is_stream_request(ctx)` 依赖 ctx.is_stream 字段。codec v2 已经在 wire 标了 stream bool;ctx 在 gw-proxy 装填时拷过来。注意 codec parse_request 后 wire 已知,但 ctx 装填顺序要在 patch_request **之前**让 adapter 可读。trait v2 ADR-AT0-9 调用顺序保证此点。
7. **gemini_cli quota probe 频率**: Google Code Assist 的 quota 重置周期是 24h(每日)。probe 频率默认 10 min(K-D)够用;但 loadCodeAssist 调用本身可能消耗 quota——需要测试确认。

---

> A-T2 在 A-T1 之上多一层 wire envelope 改写,触发 codec S2 增加一个动作型 trait 或一个新 wire 类型。
