# A-T0 — Adapter Trait v2(对齐 codec v2)（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft(v2 重写,撤回 [2026-05-19-adapter-layer-design.md](./2026-05-19-adapter-layer-design.md) 的 ADR-A2 BodyPatch 部分)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-adapter-layer-design.md](./2026-05-19-adapter-layer-design.md)(原 adapter 边界,本 spec 取代其 ADR-A1/A2/A3 三条)
  - [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md) v2(WireAny + 语义 trait 派生)
  - [2026-05-19-key-health-k-c-oauth-token.md](./2026-05-19-key-health-k-c-oauth-token.md)(OauthCache 供 adapter 调)
  - [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md)(KeyHealth 供 adapter 读)
- **范围**：升级 `UpstreamAdapter` trait 到 v2 形态:
  - 撤回 ADR-A2 的 `BodyPatch::Splice` 字节级补丁(codec v2 后 wire 不是 raw bytes);
  - 新增 `patch_request(&mut WireAnyRequest)` / `patch_response(&mut WireAnyResponse)` 钩子,通过 codec 语义 trait 改 wire;
  - `rewrite_transport` 加 `transport_profile` 概念(覆盖 A12 浏览器指纹场景的 trait 钩子,具体实现独立 spec);
  - `AdapterRegistry` 注册表 + Noop 默认实现保持。
- **不在范围**：
  - 具体 vendor 实现(→ A-T1 ~ A-T6);
  - kiro codec 形态(→ A-T7);
  - 浏览器指纹具体 HTTP client 后端选型(→ 独立 spec)。

> A-T0 是 A-T1 ~ A-T6 六个 vendor adapter 的硬前置;也是 task worker 用 adapter 的协议契约。

---

## 1. 背景

原 adapter-layer-design.md v1 ADR-A2 设计了字节级 patch:

```rust
pub enum BodyPatch<'a> {
    Splice { json_pointer: &'a str, value: serde_json::Value },
    HeaderOnly,
}
```

这是 v1 codec 还在用"borrowed body bytes"路径时的产物——把请求 body 当作 raw JSON 透传,需要打洞改字段时用 JSON pointer 单点写入。

codec v2 之后整个数据通路变了:

- 所有 wire 操作走 `WireAnyRequest` enum + 语义 trait(`HasMaxTokens`/`HasSystemPrompt`/`HasToolList` 等);
- wire 直转是编译期生成的 N² struct 互转,而不是字节级操作;
- mutation 路径(`apply_*_any`)已经统一通过 enum 分发到具体 wire 的语义 trait 方法。

继续维护 BodyPatch::Splice 等于在 codec v2 之外搞两套数据路径,违反"单一 mutation 通路"原则。**v2 撤销 BodyPatch,用 WireAny 接管所有 wire mutation**。

## 2. 设计决策

### ADR-AT0-1 新 trait 形态

```rust
// crates/gw-adapter/src/trait_def.rs

use gw_codec::wire::{WireAnyRequest, WireAnyResponse};
use gw_core::ids::*;
use std::sync::Arc;

pub trait UpstreamAdapter: Send + Sync + 'static {
    /// adapter 名称("claude_code" / "gemini_cli" / "vertex_ai" / ...)
    fn name(&self) -> &'static str;

    /// 改 auth header / OAuth bearer / SA JWT / x-api-key。
    /// 通常需要异步(OAuth refresh 可能触发)。
    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError>;

    /// 改 URL / host / TLS profile。
    /// 通常同步(可用性探测结果已缓存)。
    fn rewrite_transport(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError>;

    /// 改请求 wire——通过 WireAnyRequest 上的语义 trait 调用,而非字节 patch。
    /// 例:claude_code 注 system 调 wire.prepend_system_any(text)。
    /// 默认 no-op,vendor 自行覆写。
    fn patch_request(
        &self,
        _ctx: &AdapterCtx<'_>,
        _wire: &mut WireAnyRequest<'_>,
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    /// 改响应 wire——某些 vendor 在响应阶段也要做调整(如 codex SSE event 重写)。
    /// 默认 no-op,vendor 自行覆写。
    fn patch_response(
        &self,
        _ctx: &AdapterCtx<'_>,
        _wire: &mut WireAnyResponse<'_>,
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    /// 显式声明本 adapter 是否需要浏览器指纹(A12)。
    /// 默认 None = 不需要(走标准 reqwest);Some(profile) = 用指定 impersonate profile。
    /// gw-proxy / gw-task-runtime 在选 http client 时读这个字段。
    fn transport_profile(&self) -> Option<TransportProfile> {
        None
    }
}
```

四个钩子,加一个轻量声明字段。`patch_body` 字节版本撤销。

### ADR-AT0-2 AdapterCtx 形态

```rust
// crates/gw-adapter/src/ctx.rs

pub struct AdapterCtx<'a> {
    /// 当前请求/任务关联的 vendor 与 key 标识
    pub vendor_id: VendorId,
    pub vendor_key_id: KeyId,
    pub vendor_key: Arc<VendorKey>,  // 完整 vendor key 配置,含 quota_state(K-D)

    /// 客户端/上游协议元数据(供 vendor 内部决策)
    pub client_protocol: ProtocolKind,
    pub upstream_protocol: ProtocolKind,
    pub model: ModelName,

    /// 鉴权后用户上下文(读权限快照,不持有完整 ApiKey)
    pub user_id: UserId,
    pub api_format: ApiFormat,

    /// 共享子系统访问(只读句柄)
    pub oauth_cache: &'a Arc<OauthCache>,        // K-C
    pub health_store: &'a Arc<KeyHealthStore>,   // K-A

    /// 出站请求头/URL 改写句柄
    ///
    /// 由调用方(gw-proxy filter / gw-task-runtime)注入。
    /// adapter 通过 HeadersMut::set/remove/get 操作,不直接接触 Pingora 类型。
    pub headers_mut: HeadersMut<'a>,
    pub url_mut: UrlMut<'a>,

    /// trace_id / request_id 透传(observability)
    pub trace_id: &'a str,
    pub request_id: &'a str,
}

pub struct HeadersMut<'a> {
    inner: &'a mut dyn HeadersAccessor,
}

pub trait HeadersAccessor {
    fn get(&self, name: &str) -> Option<&str>;
    fn set(&mut self, name: &str, value: &str);
    fn remove(&mut self, name: &str);
}

pub struct UrlMut<'a> {
    inner: &'a mut dyn UrlAccessor,
}

pub trait UrlAccessor {
    fn scheme(&self) -> &str;
    fn host(&self) -> &str;
    fn path(&self) -> &str;
    fn set_host(&mut self, host: &str);
    fn set_path(&mut self, path: &str);
    fn set_scheme(&mut self, scheme: &str);
}
```

- AdapterCtx 不持有 Pingora `RequestHeader` 或 `HttpPeer`;
- HeadersMut/UrlMut trait object 由 gw-proxy 与 gw-task-runtime 各自实现适配——gw-proxy 写一个适配 `pingora::http::RequestHeader` 的 impl,gw-task-runtime 写一个适配 `reqwest::RequestBuilder` 的 impl;
- adapter 代码完全 runtime-agnostic。

### ADR-AT0-3 错误模型

```rust
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("oauth: {0}")]
    Oauth(#[from] OauthError),

    #[error("transport rewrite failed: {0}")]
    Transport(String),

    #[error("request patch failed: {0}")]
    PatchRequest(String),

    #[error("response patch failed: {0}")]
    PatchResponse(String),

    #[error("vendor configuration missing: {0}")]
    MissingConfig(&'static str),

    #[error("upstream signaled key invalid")]
    KeyInvalid,  // 上游告诉我们 key 死了,K-A 应当冷却(adapter 在 rewrite_auth 阶段就发现)
}
```

每个错误类型映射到 gw-core::error::codes 的 GW_* 错误码,由调用方(gw-proxy/gw-task-runtime)统一转译。

### ADR-AT0-4 transport_profile (A12 桶)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportProfile {
    /// 标准 reqwest,无指纹伪装
    Standard,
    /// curl_cffi 同款 Chrome impersonate
    ChromeImpersonate { version: ChromeVersion },
    /// Firefox impersonate(预留)
    FirefoxImpersonate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeVersion {
    V110, V116, V119, V120, V123, V124, V131,
}
```

`transport_profile()` 返回 None / Some,gw-proxy 与 gw-task-runtime 看到非 None 时:

- v1: warn + fallback Standard(实现未上时不阻塞);
- v2: 选 impersonate-able http client 后端发请求。

具体后端选型见独立 spec(`reqwest-impersonate` / `rustls-impersonate` 评估,本 spec 不决定)。

### ADR-AT0-5 Registry 改造

```rust
// crates/gw-adapter/src/registry.rs

pub struct AdapterRegistry {
    by_vendor: HashMap<VendorId, Arc<dyn UpstreamAdapter>>,
    default_adapter: Arc<NoopAdapter>,
    /// K-B 错误分析也走 Registry 暴露(每 vendor 独立)
    error_analyzers: HashMap<VendorId, Arc<dyn UpstreamErrorAnalyzer>>,
    default_error_analyzer: Arc<DefaultErrorAnalyzer>,
}

impl AdapterRegistry {
    pub fn for_vendor(&self, v: &VendorId) -> Arc<dyn UpstreamAdapter> {
        self.by_vendor.get(v).cloned().unwrap_or_else(|| self.default_adapter.clone())
    }

    pub fn error_analyzer_for(&self, v: &VendorId) -> Arc<dyn UpstreamErrorAnalyzer> {
        self.error_analyzers.get(v).cloned().unwrap_or_else(|| self.default_error_analyzer.clone())
    }
}
```

注册时机:gw-gateway / gw-task-worker bootstrap 阶段,按 vendor 类型注入。同款 Registry 实例同时 expose `UpstreamAdapter` 和 `UpstreamErrorAnalyzer`(K-B 已规划)。

### ADR-AT0-6 Noop 默认行为

```rust
// crates/gw-adapter/src/noop.rs

pub struct NoopAdapter;

#[async_trait::async_trait]
impl UpstreamAdapter for NoopAdapter {
    fn name(&self) -> &'static str { "noop" }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        // 标准 API key 注入:x-api-key 头
        let key = ctx.vendor_key.api_key.expose_secret();
        ctx.headers_mut.remove("authorization");
        ctx.headers_mut.set("x-api-key", key);
        Ok(())
    }

    fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        Ok(())  // 不动 URL
    }

    // patch_request / patch_response 走默认 no-op
}
```

Noop 等价于现在 gw-proxy::filters::upstream.rs 里 hard-coded 的 x-api-key 注入。本 spec 落地后 gw-proxy 把那段 hard-coded 删掉,改调 Noop adapter。

### ADR-AT0-7 在 gw-proxy 的调用点

```rust
// gw-proxy::filters::upstream::upstream_request_filter(伪代码)
async fn upstream_request_filter(&self, ctx: &mut ReqCtx, session: &Session, upstream_request: &mut RequestHeader) {
    let adapter = self.adapter_registry.for_vendor(&ctx.vendor_id);

    // 构造 AdapterCtx 适配 Pingora
    let mut headers_adapter = PingoraHeadersAdapter::new(upstream_request);
    let mut url_adapter = PingoraUrlAdapter::new(upstream_request);
    let mut actx = AdapterCtx {
        vendor_id: ctx.vendor_id.clone(),
        vendor_key_id: ctx.vendor_key_id.clone(),
        vendor_key: ctx.vendor_key.clone(),
        client_protocol: ctx.client_protocol,
        upstream_protocol: ctx.upstream_protocol,
        model: ctx.model.clone(),
        user_id: ctx.user_id.clone(),
        api_format: ctx.api_format,
        oauth_cache: &self.oauth_cache,
        health_store: &self.health_store,
        headers_mut: HeadersMut { inner: &mut headers_adapter },
        url_mut: UrlMut { inner: &mut url_adapter },
        trace_id: &ctx.trace_id.0,
        request_id: &ctx.request_id.0,
    };

    adapter.rewrite_auth(&mut actx).await?;
    adapter.rewrite_transport(&mut actx)?;
    adapter.patch_request(&actx, &mut ctx.wire)?;  // wire 改写
    // 注:patch_response 在 response_filter 调用,这里不动
}
```

### ADR-AT0-8 与 task worker 的复用

gw-task-runtime 调用 adapter 形态:

```rust
// gw-task-runtime::executor(伪代码)
let adapter = self.adapter_registry.for_vendor(&task.vendor_id);

let mut req_builder = reqwest::Client::new().request(method, url);
let mut headers_adapter = ReqwestHeadersAdapter::new(&mut req_builder);
let mut url_adapter = ReqwestUrlAdapter::new(&mut req_builder);
let mut actx = AdapterCtx { /* 同上,只是 oauth_cache/health_store 引用换成 task-side */ };

adapter.rewrite_auth(&mut actx).await?;
adapter.rewrite_transport(&mut actx)?;
adapter.patch_request(&actx, &mut wire)?;
let response = req_builder.send().await?;
// 响应回路:
let mut resp_wire = ...;
adapter.patch_response(&actx, &mut resp_wire)?;
```

同一份 adapter trait,两端复用——这是 adapter 提到 gw-adapter 独立 crate 的核心动机。

### ADR-AT0-9 调用顺序约定

固定顺序:`rewrite_auth → rewrite_transport → patch_request →(发请求)→ patch_response`。

为什么:

- rewrite_auth 先,因为它可能 await OAuth refresh;
- rewrite_transport 紧接,可能基于 auth 结果改 URL(vertex_ai 用 project_id 做 URL,project_id 在 SA token 里);
- patch_request 最后,wire 已经稳定,改 system / 注 thinking_config 等;
- patch_response 在响应路径上独立调,与请求路径解耦。

### ADR-AT0-10 性能预算

| 操作 | 预算(P50) |
|---|---|
| `for_vendor` lookup | < 30 ns(HashMap get + Arc clone) |
| Noop `rewrite_auth` | < 200 ns(header set) |
| OAuth `rewrite_auth` cache hit | < 500 ns(K-C 已规划) |
| OAuth `rewrite_auth` refresh(cold) | < 500 ms(上游 OAuth endpoint) |
| `rewrite_transport` 简单 URL 改 | < 100 ns |
| `patch_request` 简单 wire 改(注 system) | < 200 ns |

热路径预算 < 2 µs(Noop 路径)。OAuth refresh 是 cold path,单独评估。

## 3. 实施大纲

1. **adapter-layer-design.md** 同 PR 加 v2 修订说明,撤回 ADR-A2 BodyPatch;
2. `crates/gw-adapter/src/trait_def.rs` 实施新 trait 签名;
3. `ctx.rs` 实施 AdapterCtx + HeadersMut/UrlMut trait;
4. `noop.rs` 实施 NoopAdapter;
5. `registry.rs` 实施 AdapterRegistry(含 error analyzer 复用,与 K-B 对齐);
6. `gw-proxy` 实施 PingoraHeadersAdapter / PingoraUrlAdapter(适配层);
7. `gw-proxy::filters::upstream` 删除 hard-coded x-api-key 注入,改调 Noop adapter;
8. (task worker spec 落地后)gw-task-runtime 实施 ReqwestHeadersAdapter / ReqwestUrlAdapter;
9. 单测:Noop 行为等价于原 hard-coded;Registry lookup;HeadersMut 适配 round-trip;
10. 性能 bench 守 §ADR-AT0-10 预算。

## 4. 未决问题

1. **async_trait 性能**: rewrite_auth 必须 async(OAuth),但 trait async fn 默认走 dyn dispatch + Box::pin。预算 200 ns 可能边缘。倾向 v1 用 `async_trait` crate,bench 出来再决定是否换 GAT-based 自实现。
2. **AdapterCtx 包含 Arc<VendorKey>**: 持有完整 vendor key(含 secret api_key)。adapter 实现里若误写 log(`{ctx:?}`) 会泄密。倾向 VendorKey::Debug 已经 redact api_key(已实现),AdapterCtx::Debug 复用。但需要 review 严格。
3. **HeadersMut/UrlMut trait object 性能**: vtable 间接调用 ~ 5 ns,可接受。但对每个请求多分配一个 trait object,GC 压力?倾向用 enum 替代 dyn:`HeadersMut::Pingora(...) | HeadersMut::Reqwest(...)`,栈分配。trade-off 是 gw-adapter crate 要 import 两端的类型——违反 runtime-agnostic 原则。倾向保留 dyn,后续 benches 实测再优化。
4. **patch_request 修改 wire 后如何标记 Mutations**: codec v2 的 Mutations bit 用于 passthrough fast path 判定。adapter 改 wire 后必须 set 对应 bit(`Mutations::set_system_prompt`)。是否在 patch_request 签名加 `&mut Mutations`?倾向 adapter 通过 `wire.prepend_system_any()`(`apply_*_any` helpers)自动 set bit——helper 内部已处理,adapter 透明。
5. **error_analyzer 与 adapter 的关系**: 当前 Registry 同时持两者,但 trait 分开。是否应该合并到一个 trait `Vendor`?倾向**不合并**:adapter 是数据通路改写,error_analyzer 是错误响应分析,职责不同;Vendor 同名概念在 entity 层已有歧义。保持分离。
6. **transport_profile 的实际启用时机**: v1 trait 上声明,但 gw-proxy / gw-task-runtime 是否真用?倾向 v1 不启用(adapter 都返回 None;具体 impersonate 实现独立 spec);trait 保留为 forward-compat。这条在 CLAUDE.md / gap-inventory 标注 A12 状态。
7. **adapter 是否要接 K-A 错误标记**: adapter 在 rewrite_auth 阶段发现 token 永久失效(AccountBlocked)→ 直接 health_store.apply_cooldown?还是返回 AdapterError::KeyInvalid 让调用方处理?倾向**返 Error,调用方决定**——保持 adapter 无副作用(易测试),调用方在 K-B 分析钩子里统一处理 cooldown。

---

> A-T0 是 6 个 vendor adapter 的硬前置。A-T1 ~ A-T6 写时直接复用本 spec 的 trait 与 ctx,vendor 自身职责清晰。kiro 因涉及新 codec,vendor adapter 拆为 A-T6(adapter shell),codec 部分独立 A-T7。
