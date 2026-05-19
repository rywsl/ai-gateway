# A-T6 — kiro Adapter Shell(v2026-05-19)

- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-adapter-a-t0-trait-v2.md](./2026-05-19-adapter-a-t0-trait-v2.md)(**硬前置**)
  - [2026-05-19-codec-s5-kiro-eventstream.md](./2026-05-19-codec-s5-kiro-eventstream.md)(**硬前置**:Kiro 专用 codec,A-T7)
  - Aether 来源:`src/services/provider/adapters/kiro/`
- **范围**：Kiro 上游 adapter **shell**(壳):
  - rewrite_auth:AWS SigV4 鉴权 OR token-based(取决于 vendor_key 配置);
  - rewrite_transport:URL → `/generateAssistantResponse`;region 选择(AWS region);
  - patch_request:wire 上的 Kiro envelope 包装(claude-on-kiro 走 Anthropic 协议但 body 形态不同);
  - patch_response:**无在 adapter 层做**——AWS EventStream 二进制流解码全部归 A-T7 codec;
  - transport_profile:**ChromeImpersonate**(Kiro 上游会嗅探客户端指纹)。
- **不在范围**：
  - AWS EventStream 二进制协议解析(→ A-T7);
  - KiroResponse / KiroStreamFrame wire 形态(→ A-T7);
  - thinking tag 解析与 buffer(→ A-T7);
  - AWS SigV4 算法实现(→ 本 spec 引用 `aws-sigv4` crate);
  - Kiro 账号 OAuth token 管理(若 token 模式,复用 K-C)。

> A-T6 是 6 个 vendor 里**最特殊**的——它的复杂度主要在 codec 层(A-T7),adapter 本身相对薄,但需要正确接 codec 与 transport profile。

---

## 1. 背景

Kiro 是 AWS 推出的 Claude 反代/打包服务(KiroIDE 产品的后端)。它的特殊性:

- **响应流是 AWS EventStream 二进制格式**(`application/vnd.amazon.eventstream`),不是标准 SSE;
- **请求形态是 Anthropic Messages 兼容**,但顶层加了 Kiro 特有的 envelope 字段;
- 鉴权可能走 AWS SigV4(企业 Bedrock 通路)或 Kiro 自己的 OAuth token(KiroIDE 个人通路);
- 客户端**有指纹检测**(模仿 KiroIDE 0.8.0 + Node 22 + 平台版本字符串);
- 响应里 `<thinking>` 标签是文本中嵌入的(不是结构化字段),需要 codec 层做 tag 解析。

Aether 用 `kiro/parser/decoder.py` 解 EventStream,`eventstream_rewriter.py` 把它改写成 Claude SSE 给客户端。Rust 端的拆分:

- **A-T6 (本 spec)**:adapter shell —— 鉴权、URL、headers、envelope 包装;
- **A-T7 (codec s5)**:wire 与流式解析 —— EventStream framer + KiroResponse wire + tag parser。

## 2. 设计决策

### ADR-AT6-1 vendor 注册

```
vendor_id = "kiro"
```

### ADR-AT6-2 鉴权两模式

```rust
pub enum KiroAuthMode {
    /// AWS SigV4 鉴权(企业 Bedrock 模式)。vendor_key.auth_config 存 AWS access_key_id + secret + region。
    AwsSigV4,
    /// Kiro OAuth token(个人模式)。vendor_key.auth_config 存 refresh_token。
    OauthToken,
}
```

`VendorKey::auth_type` 区分。

### ADR-AT6-3 constants(同步 Aether)

```rust
// crates/gw-adapter/src/kiro/constants.rs

pub const AWS_EVENTSTREAM_CONTENT_TYPE: &str = "application/vnd.amazon.eventstream";
pub const KIRO_GENERATE_ASSISTANT_PATH: &str = "/generateAssistantResponse";
pub const KIRO_USAGE_LIMITS_PATH: &str = "/getUsageLimits";
pub const DEFAULT_REGION: &str = "us-east-1";

pub const DEFAULT_KIRO_VERSION: &str = "0.8.0";
pub const DEFAULT_NODE_VERSION: &str = "22.21.1";

pub fn detect_system_version() -> String {
    // Rust 等价 Python platform.system()/platform.release()
    let system = std::env::consts::OS;  // "linux"/"macos"/"windows"
    // release 信息:Linux 走 uname,其他平台 fallback
    let release = uname_release().unwrap_or_else(|| "unknown".into());
    format!("{}#{}", system, release)
}
```

### ADR-AT6-4 headers 伪装

Kiro 嗅探客户端为 KiroIDE。adapter 注入:

```rust
fn build_kiro_headers(vendor_key: &VendorKey) -> Vec<(String, String)> {
    let system_version = constants::detect_system_version();
    vec![
        ("Content-Type".into(), constants::AWS_EVENTSTREAM_CONTENT_TYPE.into()),
        ("Accept".into(), "application/vnd.amazon.eventstream".into()),
        ("User-Agent".into(), format!("KiroIDE/{}", constants::DEFAULT_KIRO_VERSION)),
        ("X-Kiro-Client-Version".into(), constants::DEFAULT_KIRO_VERSION.into()),
        ("X-Kiro-Node-Version".into(), constants::DEFAULT_NODE_VERSION.into()),
        ("X-Kiro-System".into(), system_version),
        // ... 其他 fingerprint 头,从 Aether `headers.py` 同步
    ]
}
```

### ADR-AT6-5 KiroAdapter 实现

```rust
// crates/gw-adapter/src/kiro/adapter.rs

pub struct KiroAdapter {
    refresher: Arc<dyn OAuthRefresher>,  // OAuth 模式用
}

#[async_trait]
impl UpstreamAdapter for KiroAdapter {
    fn name(&self) -> &'static str { "kiro" }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let mode = read_auth_mode(&ctx.vendor_key)?;
        match mode {
            KiroAuthMode::OauthToken => {
                let token = ctx.oauth_cache.get_access_token(&ctx.vendor_key, &*self.refresher).await?;
                ctx.headers_mut.set("authorization", &format!("Bearer {}", token.expose_secret()));
            }
            KiroAuthMode::AwsSigV4 => {
                // SigV4 需要在请求 body 完整后再签——这里只准备 credentials,
                // 实际 SigV4 计算放 gw-proxy::filters::upstream send 之前的 hook 里。
                // adapter 在 ctx.metadata 上放 access_key / secret / region 让 gw-proxy 用。
                let creds = read_aws_credentials(&ctx.vendor_key)?;
                ctx.metadata.insert("kiro.aws_access_key_id".into(), creds.access_key_id);
                ctx.metadata.insert("kiro.aws_secret_key".into(), creds.secret_key.expose_secret().to_owned());
                ctx.metadata.insert("kiro.aws_region".into(), creds.region);
            }
        }
        // 注入伪装头
        for (k, v) in build_kiro_headers(&ctx.vendor_key) {
            ctx.headers_mut.set(&k, &v);
        }
        Ok(())
    }

    fn rewrite_transport(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let region = read_aws_region(&ctx.vendor_key)
            .unwrap_or_else(|| constants::DEFAULT_REGION.to_owned());
        // Kiro 上游 host(具体值需 Aether 同步)
        let host = format!("kiro.{region}.amazonaws.com");
        ctx.url_mut.set_scheme("https");
        ctx.url_mut.set_host(&host);
        ctx.url_mut.set_path(constants::KIRO_GENERATE_ASSISTANT_PATH);
        Ok(())
    }

    fn patch_request(&self, _ctx: &AdapterCtx<'_>, wire: &mut WireAnyRequest<'_>)
        -> Result<(), AdapterError>
    {
        // Kiro envelope wrap:在 Anthropic 请求基础上加 Kiro 顶层字段
        // (具体字段从 Aether envelope.py 同步)
        // wire.wrap_for_kiro_any()——codec S2 配套加 trait
        wire.wrap_for_kiro_any().map_err(|e| AdapterError::PatchRequest(e.to_string()))?;
        Ok(())
    }

    fn patch_response(&self, _ctx: &AdapterCtx<'_>, _wire: &mut WireAnyResponse<'_>)
        -> Result<(), AdapterError>
    {
        // 响应 wire(KiroResponse)由 codec A-T7 解码后直接是 Anthropic 兼容形态,
        // adapter 在这里**通常 no-op**。
        // 例外:thinking tag 整流(Aether `eventstream_rewriter.py` 的 thinking buffer)
        // 这条逻辑放 A-T7 codec 内更合理(它是 wire 解析的一部分,不是 adapter 改写)。
        Ok(())
    }

    fn transport_profile(&self) -> Option<TransportProfile> {
        Some(TransportProfile::ChromeImpersonate {
            version: ChromeVersion::V131,
        })
    }
}
```

### ADR-AT6-6 AWS SigV4 实施

SigV4 是 AWS 标准 4 步签名(canonical request → string to sign → signing key → signature)。Rust 用 `aws-sigv4` crate(官方 SDK 一部分):

```rust
// 实施时机:gw-proxy 在 send 请求前
fn apply_sigv4(req: &mut Request, ctx: &AdapterCtx) -> Result<(), AdapterError> {
    let access_key = ctx.metadata.get("kiro.aws_access_key_id")?;
    let secret_key = ctx.metadata.get("kiro.aws_secret_key")?;
    let region = ctx.metadata.get("kiro.aws_region")?;

    let signing_params = aws_sigv4::http_request::SigningParams::builder()
        .access_key(access_key)
        .secret_key(secret_key)
        .region(region)
        .service_name("execute-api")  // Kiro 实际 service 名待确认
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()?;

    let signable_request = SignableRequest::new(req)?;
    let (signing_instructions, _signature) = aws_sigv4::http_request::sign(signable_request, &signing_params)?;
    signing_instructions.apply_to_request(req);
    Ok(())
}
```

这是 gw-proxy 的 hook(adapter 完成 rewrite_auth 后,gw-proxy 在 send 前查 ctx.metadata 是否有 sigv4 credentials,有则签名)。

⚠️ **A-T0 trait v2 需要支持 SigV4 这种"body 完整后再签"模式**。两种方式:
- (X) 加 `post_body_hook(ctx: &AdapterCtx, body: &[u8]) -> headers_to_add`;
- (Y) gw-proxy 直接读 ctx.metadata 应用 SigV4(adapter 不感知);

倾向 (Y):SigV4 不是 adapter 概念,是 transport-layer 加密签名;放 ctx.metadata + gw-proxy 处理更干净。adapter trait 不增 hook。

### ADR-AT6-7 token refresh

Kiro OAuth refresh endpoint(具体 URL 从 Aether `token_manager.py` 同步):

```rust
pub struct KiroRefresher {
    http: reqwest::Client,
    token_url: String,
}

// 实现同 A-T1/A-T2,POST refresh_token grant,解析 access_token + expires_in。
```

### ADR-AT6-8 K-B / K-D

- **K-B**: KiroErrorAnalyzer 复用 default 大部分。**特例**:Kiro 错误响应也是 EventStream binary,需要先解码再分析关键词。但 default analyzer 只看 body_preview(前 4KB raw 字节),对 EventStream 二进制无效。两种解决:
  - (M1) K-B 给 vendor analyzer 加一个 hook,让 vendor 自行解析 body_preview;
  - (M2) gw-proxy 在收完错误响应后,如果 Content-Type 是 EventStream,先用 A-T7 decoder 解码再传给 K-B。

  倾向 (M2):codec 已经会解 EventStream,gw-proxy 一句 if-content-type 即可。

- **K-D**: Kiro 有 `/getUsageLimits` endpoint 返回 quota。`KiroQuotaProber` 调它解析(参 K-D Aether `usage.py`)。Aether 已实现完整,Rust 等价移植。

### ADR-AT6-9 测试

- rewrite_auth OAuth 模式 → Bearer 设置 + Kiro 伪装头;
- rewrite_auth SigV4 模式 → ctx.metadata 含 credentials;
- rewrite_transport URL 拼正确(region 默认/自定义);
- patch_request envelope wrap 后 body 含 Kiro 顶层字段(参 A-T7);
- transport_profile 返回 ChromeImpersonate;
- E2E(integration test):mock Kiro endpoint 接收 SigV4 请求,验证签名正确。

## 3. 实施大纲

依赖:A-T0 land + A-T7 (codec s5) land + K-A/K-B/K-C/K-D land + `aws-sigv4` crate 新增。

1. `crates/gw-adapter/src/kiro/` 模块;
2. constants.rs / auth_mode.rs / adapter.rs / refresher.rs / error_analyzer.rs;
3. gw-proxy SigV4 hook(读 ctx.metadata 应用签名);
4. K-B 路径:gw-proxy 检测 EventStream Content-Type 时先解码再传给 analyzer;
5. workspace `Cargo.toml` 加 `aws-sigv4`(或 `aws-sdk-bedrockruntime`-light)依赖;
6. 单测覆盖。

## 4. 未决问题

1. **Kiro 实际上游 host 与 service 名**: Aether 代码里的具体值需要实施时同步。SigV4 service name 决定签名行为。
2. **OAuth token refresh endpoint**: 同上,Aether 同步。
3. **AWS SigV4 vs Bearer 两条路径选择策略**: vendor_key 必须显式标 auth_mode 字段,**不靠嗅探**(避免误判)。Vendor entity / VendorKey schema 加 `auth_mode: enum` 字段。
4. **transport_profile 实施时机**: A-T0 加了字段但 v1 不强制启用。Kiro 是首个**真的需要** ChromeImpersonate 的 vendor——若不上 impersonate-able HTTP client,Kiro 在某些 Cloudflare 防护下会被拒。这给 transport_profile 实施加了优先级。倾向独立 spec 处理 transport_profile,且优先级 P1(Kiro 需要)。
5. **`/getUsageLimits` 调用频率**: K-D probe 间隔默认 10 min。Kiro 调用本身可能消耗 quota——需测试确认。
6. **EventStream Content-Type 在错误响应**: 实测 Kiro 错误时返回 EventStream 还是普通 JSON 错误?若分情况,K-B 路径要更复杂。倾向先实现 (M2),发现需要扩再说。
7. **多 region 支持**: Kiro 在不同 region 部署。VendorKey.region 字段 v1 必需。若同账号支持多 region,需要 K-A 按 (KeyId, ApiFormat, Region) 分桶。倾向 v1 不支持多 region(一个 VendorKey 绑定一个 region)。

---

> A-T6 是 adapter shell;A-T7 是真正的复杂部分(codec)。两者一起完成 Kiro 上游支持。
