# A-T3 — vertex_ai Adapter(v2026-05-19)

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-adapter-a-t0-trait-v2.md](./2026-05-19-adapter-a-t0-trait-v2.md)(**硬前置**)
  - Aether 来源:`src/services/provider/adapters/vertex_ai/`
- **范围**：Google Vertex AI 上游 adapter:
  - rewrite_auth:Service Account JSON → JWT 签名 → Access Token → Bearer 头(JWT 签名是核心工作);或 API Key 走 URL `?key=` 参数;
  - rewrite_transport:按 (auth_mode, model_family, region) 三元 lookup 拼 URL(共 4 种 URL 模板);
  - patch_request:无(Vertex 用标准 Gemini / Anthropic envelope,与 codec 输出一致);
  - patch_response:无;
  - transport_profile:无。
- **不在范围**：
  - SA JSON 录入流程(Go 管理面);
  - 各模型 region 映射表的初始数据(由 Go 写入 entity);
  - claude on vertex 与 anthropic 直连的代码路径分歧(由 codec 选择 ProtocolKind::Anthropic 时已处理)。

> A-T3 是 6 个 vendor 里**算法上最复杂**的:JWT 签名(RS256)+ Google OAuth2 token exchange + URL 模板拼接。Rust 端实现工作量主要在 jwt 签名,其他都是字符串操作。

---

## 1. 背景

Vertex AI 暴露 Gemini / Anthropic 等模型,但**有两种鉴权模式**:

1. **API Key 模式**(Express mode):
   - URL: `https://aiplatform.googleapis.com/v1/publishers/google/models/{model}:{action}?key={api_key}`
   - 仅支持 Gemini / Imagen;
   - Anthropic 不支持此模式。

2. **Service Account 模式**:
   - URL: `https://{region}-aiplatform.googleapis.com/v1/projects/{project_id}/locations/{region}/publishers/{publisher}/models/{model}:{action}`
   - publisher = `google`(Gemini)或 `anthropic`(Claude on Vertex);
   - 鉴权用 OAuth2 Bearer(从 SA JWT 换出来的 access_token);
   - 必须按区域(region)拼 host,因为 Vertex 的模型部署在特定 region。

adapter 在 rewrite_transport 里要根据 vendor_key 配置 + 模型 + 区域决定 URL 模板;rewrite_auth 里要做 JWT 签名 → access token exchange。

## 2. 设计决策

### ADR-AT3-1 vendor 注册

```
vendor_id = "vertex_ai"
```

### ADR-AT3-2 鉴权两模式

```rust
// crates/gw-adapter/src/vertex_ai/auth_mode.rs

pub enum VertexAuthMode {
    /// API Key 模式 - vendor_key.api_key 是 API key 字符串
    ApiKey,
    /// Service Account 模式 - vendor_key.auth_config 是 SA JSON
    ServiceAccount,
}
```

`VendorKey::auth_type`(已存在或新加)字段区分。Aether 在 `provider_auth_types.py` 里有 `ProviderAuthInfo`,Rust 这里用 enum 表达。

### ADR-AT3-3 SA JSON 形态

```rust
#[derive(Deserialize)]
pub struct ServiceAccountJson {
    #[serde(rename = "type")]
    pub r#type: String,            // "service_account"
    pub project_id: String,
    pub private_key_id: String,
    pub private_key: String,       // PEM-encoded RSA private key
    pub client_email: String,
    pub token_uri: String,         // 通常 "https://oauth2.googleapis.com/token"
    pub auth_provider_x509_cert_url: Option<String>,
    pub client_x509_cert_url: Option<String>,
}
```

存储:vendor_key.auth_config 字段(KMS-加密 JSON)。VendorKey 字段已经支持(K-C 顺手补);Vertex adapter 在 rewrite_auth 时解密读取。

### ADR-AT3-4 JWT 生成 + token exchange

OAuth2 Service Account flow:

```
1. 生成 JWT claim:
   {
     "iss": <client_email>,
     "scope": "https://www.googleapis.com/auth/cloud-platform",
     "aud": <token_uri>,
     "exp": now + 3600,
     "iat": now
   }
2. 用 SA 的 private_key 做 RS256 签名 → JWT;
3. POST <token_uri>:
     grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer
     assertion=<jwt>
4. 上游返回 access_token + expires_in;
5. 缓存 access_token,Bearer 注头。
```

实现:

```rust
// crates/gw-adapter/src/vertex_ai/jwt.rs

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};

#[derive(Serialize)]
struct JwtClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    exp: u64,
    iat: u64,
}

pub fn build_jwt(sa: &ServiceAccountJson) -> Result<String, AdapterError> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let claims = JwtClaims {
        iss: &sa.client_email,
        scope: "https://www.googleapis.com/auth/cloud-platform",
        aud: &sa.token_uri,
        exp: now + 3600,
        iat: now,
    };
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(sa.private_key_id.clone());

    let key = EncodingKey::from_rsa_pem(sa.private_key.as_bytes())
        .map_err(|e| AdapterError::Oauth(OauthError::Permanent(format!("invalid SA private_key: {e}"))))?;

    encode(&header, &claims, &key)
        .map_err(|e| AdapterError::Oauth(OauthError::Permanent(format!("jwt sign: {e}"))))
}
```

依赖 `jsonwebtoken` crate(成熟 + 与 ring 集成)。

### ADR-AT3-5 access token 缓存复用 K-C

vertex_ai 的 access_token 同 OAuth bearer,**复用 K-C OauthCache**:

```rust
pub struct VertexAiRefresher {
    http: reqwest::Client,
}

#[async_trait]
impl OAuthRefresher for VertexAiRefresher {
    async fn refresh(&self, _current_refresh_token: &SecretString) -> Result<RefreshedToken, RefreshError> {
        // ⚠️ vertex_ai 没有 refresh_token 概念——每次都重生成 JWT。
        // 这里把 SA JSON 当 "refresh_token" 看待:current_refresh_token 是 base64 编码的 SA JSON。
        // 不优雅但能复用 OauthCache 接口。
        let sa: ServiceAccountJson = serde_json::from_str(current_refresh_token.expose_secret())
            .map_err(|e| RefreshError::Permanent { detail: format!("SA decode: {e}") })?;

        let jwt = build_jwt(&sa)
            .map_err(|e| RefreshError::Permanent { detail: e.to_string() })?;

        let resp = self.http
            .post(&sa.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", &jwt),
            ])
            .send()
            .await
            .map_err(|e| RefreshError::Transient { detail: e.to_string() })?;

        // 解析响应,返回 RefreshedToken(refresh_token 字段留空——SA flow 不旋转 refresh_token)
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        match status.as_u16() {
            200 => {
                let parsed: GoogleTokenResponse = serde_json::from_str(&text)
                    .map_err(|e| RefreshError::Permanent { detail: format!("decode: {e}") })?;
                Ok(RefreshedToken {
                    access_token: SecretString::new(parsed.access_token),
                    refresh_token: None,  // SA flow 不旋转
                    expires_in_secs: parsed.expires_in,
                })
            }
            401 | 403 => {
                if text.contains("invalid_grant") || text.contains("disabled") {
                    Err(RefreshError::AccountBlocked { detail: text })
                } else {
                    Err(RefreshError::Permanent { detail: text })
                }
            }
            429 | 500..=599 => Err(RefreshError::Transient { detail: text }),
            _ => Err(RefreshError::Permanent { detail: text }),
        }
    }
}

#[derive(Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    expires_in: u32,
    // token_type: String, // "Bearer"
}
```

K-C OauthCache 把 SA JSON 当 "refresh_token" 存——稍丑,但形态统一。

### ADR-AT3-6 URL 模板

```rust
// crates/gw-adapter/src/vertex_ai/url_template.rs

pub fn build_url(
    auth_mode: VertexAuthMode,
    sa_project: Option<&str>,
    region: &str,
    publisher: &str,    // "google" or "anthropic"
    model: &str,
    action: &str,        // "generateContent" / "streamGenerateContent" / "predict" / ...
    api_key: Option<&str>,
) -> Result<(String, String), AdapterError> {
    match (auth_mode, publisher) {
        (VertexAuthMode::ApiKey, "google") => {
            let host = "aiplatform.googleapis.com".to_owned();
            let path = format!("/v1/publishers/google/models/{model}:{action}");
            let path_with_query = format!("{path}?key={}", api_key.unwrap_or(""));
            Ok((host, path_with_query))
        }
        (VertexAuthMode::ApiKey, _) => Err(AdapterError::MissingConfig(
            "vertex_ai API key mode only supports Google publishers"
        )),
        (VertexAuthMode::ServiceAccount, _) => {
            let project = sa_project.ok_or(AdapterError::MissingConfig("missing project_id"))?;
            let host = format!("{region}-aiplatform.googleapis.com");
            let path = format!(
                "/v1/projects/{project}/locations/{region}/publishers/{publisher}/models/{model}:{action}"
            );
            Ok((host, path))
        }
    }
}
```

publisher 由模型决定:`gemini-*` → google,`claude-*` → anthropic。codec 已经知道 protocol(ProtocolKind),adapter 据此推断:

```rust
fn publisher_for(model: &ModelName) -> &'static str {
    if model.0.starts_with("claude") { "anthropic" } else { "google" }
}
```

region 由 vendor_key 配置(metadata `vertex_ai.region` 或默认表):

```rust
// crates/gw-adapter/src/vertex_ai/regions.rs

pub const DEFAULT_MODEL_REGIONS: &[(&str, &str)] = &[
    ("gemini-2.0-flash", "us-central1"),
    ("claude-3-5-sonnet-20240620", "us-east5"),
    ("claude-3-7-sonnet-20250219", "us-east5"),
    // ...
];
```

更新策略:default 表过期不影响功能(Vertex 仍按上次成功的 region 工作)——但模型 GA 到新 region 时需要更新。倾向把这张表挪到 Go 管理面 entity,Rust 读;v1 先 const,follow-up 改成 entity。

### ADR-AT3-7 VertexAiAdapter 实现

```rust
pub struct VertexAiAdapter {
    refresher: Arc<VertexAiRefresher>,
}

#[async_trait]
impl UpstreamAdapter for VertexAiAdapter {
    fn name(&self) -> &'static str { "vertex_ai" }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let auth_mode = read_auth_mode(&ctx.vendor_key)?;
        match auth_mode {
            VertexAuthMode::ApiKey => {
                // API Key 走 URL 查询参数,这里不动 header
                ctx.headers_mut.remove("authorization");
                ctx.headers_mut.remove("x-goog-api-key");
                // headers 不设;URL 在 rewrite_transport 里拼 ?key=
                Ok(())
            }
            VertexAuthMode::ServiceAccount => {
                let token = ctx.oauth_cache.get_access_token(&ctx.vendor_key, &*self.refresher).await?;
                ctx.headers_mut.remove("x-api-key");
                ctx.headers_mut.set("authorization", &format!("Bearer {}", token.expose_secret()));
                Ok(())
            }
        }
    }

    fn rewrite_transport(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let auth_mode = read_auth_mode(&ctx.vendor_key)?;
        let publisher = publisher_for(&ctx.model);
        let region = read_region(&ctx.vendor_key, &ctx.model)?;
        let action = match (ctx.upstream_protocol, ctx.is_stream) {
            (ProtocolKind::Gemini, true) => "streamGenerateContent",
            (ProtocolKind::Gemini, false) => "generateContent",
            (ProtocolKind::Anthropic, true) => "streamRawPredict",
            (ProtocolKind::Anthropic, false) => "rawPredict",
            _ => return Err(AdapterError::PatchRequest("vertex_ai requires gemini or anthropic upstream".into())),
        };
        let (sa_project, api_key) = match auth_mode {
            VertexAuthMode::ApiKey => (None, Some(ctx.vendor_key.api_key.expose_secret().as_str())),
            VertexAuthMode::ServiceAccount => {
                let sa: ServiceAccountJson = read_sa_json(&ctx.vendor_key)?;
                (Some(sa.project_id.as_str().to_owned()), None)
            }
        };
        let (host, path) = build_url(auth_mode, sa_project.as_deref(), &region, publisher, &ctx.model.0, action, api_key)?;
        ctx.url_mut.set_scheme("https");
        ctx.url_mut.set_host(&host);
        ctx.url_mut.set_path(&path);
        Ok(())
    }

    // patch_request / patch_response no-op
}
```

### ADR-AT3-8 K-B 错误分析

vertex_ai 特化:

```rust
impl UpstreamErrorAnalyzer for VertexAiErrorAnalyzer {
    fn analyze(&self, ctx: &ErrorAnalyzeCtx<'_>) -> ErrorOutcome {
        match ctx.http_status {
            403 if ctx.body_preview.contains_pattern(b"SERVICE_DISABLED") => ErrorOutcome {
                // SA 项目没启用 Vertex API
                cooldown: Some(CooldownDirective {
                    reason: CooldownReason::Status403AccountSuspended,
                    ttl_secs: 3600,
                    reset_consecutive: false,
                }),
                oauth_signal: None,
                count_as_failure: true,
            },
            403 if ctx.body_preview.contains_pattern(b"PERMISSION_DENIED") &&
                   ctx.body_preview.contains_pattern(b"quota") => ErrorOutcome {
                cooldown: Some(CooldownDirective {
                    reason: CooldownReason::Status429RateLimit,
                    ttl_secs: 60,
                    reset_consecutive: false,
                }),
                oauth_signal: None,
                count_as_failure: true,
            },
            401 => ErrorOutcome {
                cooldown: None,
                oauth_signal: Some(OAuthSignal::TokenExpired),  // SA token 过期
                count_as_failure: false,
            },
            _ => DefaultErrorAnalyzer.analyze(ctx),
        }
    }
}
```

### ADR-AT3-9 测试

- jwt 签名:用已知 SA JSON 与时间戳验签输出与 google-auth-library 等价;
- url 模板:每种 (auth_mode × publisher × stream) 组合 → URL 正确;
- rewrite_auth API Key 模式 → 不设 authorization 头;
- rewrite_auth SA 模式 → Bearer 设置;
- 错误分析 403 SERVICE_DISABLED → 长冷却;
- refresher 失败分类正确;
- region 缺失时报 MissingConfig。

## 3. 实施大纲

依赖:A-T0 + K-A/K-B/K-C land + `jsonwebtoken` crate 新增依赖。

1. `crates/gw-adapter/src/vertex_ai/` 模块;
2. constants.rs(default region 表);
3. jwt.rs(SA → JWT);
4. auth_mode.rs(枚举 + 读取);
5. url_template.rs(4 种 URL);
6. adapter.rs;
7. refresher.rs;
8. error_analyzer.rs;
9. workspace `Cargo.toml` 加 `jsonwebtoken = "9"` 依赖;
10. 单测覆盖。

## 4. 未决问题

1. **default region 表是 const 还是 entity**: v1 const(改动需 deploy);v2 推到 entity(Go 管理面 UI 改)。倾向 v1 const + 一个 entity override(`entity:vertex_region_override:<model>` 优先)。
2. **SA JSON 加密存储**: 已经走 KMS(参 ADR-017),无新工作。但 SA JSON 大(~1-2KB),加密后 entity 值约 2-3KB,需要 schema 兼容性确认。
3. **JWT cache window**: K-C OauthCache 已经做了 (`expires_in - 60s`)。vertex SA token 通常 expires_in = 3600,余量 60s 够。
4. **多 region vendor_key**: 一个 SA 通常可以跨 region 使用;但 quota 是 region 级别的。是否要把同一 SA 在不同 region 当作不同 KeyHealth 维度?倾向 v1 不分,K-A 按 (KeyId, ApiFormat) 已经分桶够用;若 region 跨 quota 隔离很严,后续在 K-A 加 region 维度。
5. **Claude on Vertex 模型版本映射**: Anthropic 在 Vertex 上的模型名跟直连 Anthropic 不一样(claude-3-5-sonnet@20240620 vs claude-3-5-sonnet-20240620)。codec 输出哪个由 user 请求决定——但 Vertex 上必须是 @ 形态。倾向在 patch_request 里加 model 名转写;或者让 codec render 时知道目标 vendor。后者更干净但需要 codec 配合,留 follow-up。
6. **JWT 签名 cold path 性能**: RS256 签名 ~ 1-2 ms(ring impl)。在 OauthCache miss 时进入,1 小时一次,可接受。但如果 OauthCache 失效大批量同时进入(N 个并发请求第一次访问)→ N 个 JWT 同时签 → CPU 突发。倾向 K-C 的 CAS 已经阻止同 key 并发 refresh,N 个 user 各自独立的 cold path 仍有,但实际并发数可控。
7. **proxy 配置**: Aether `_get_proxy_config` 让 SA token exchange 走代理(国内访问 Google)。Rust v1 不做代理(reqwest 默认 system proxy 即可)。后续 vendor_key.proxy 字段(已存在)可读,但 token exchange 调用是否要走 proxy 还是直连——倾向走 proxy(与请求路径一致)。

---

> A-T3 复杂在 JWT 与 URL 模板,但接入 K-C OauthCache 后流程清晰。下一份 A-T4 antigravity 是另一种风格(URL 探测 + 签名缓存)。
