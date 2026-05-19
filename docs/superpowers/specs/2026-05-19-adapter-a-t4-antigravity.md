# A-T4 — antigravity Adapter(v2026-05-19)

- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-adapter-a-t0-trait-v2.md](./2026-05-19-adapter-a-t0-trait-v2.md)(**硬前置**)
  - Aether 来源:`src/services/provider/adapters/antigravity/`
- **范围**：Antigravity 上游 adapter:
  - rewrite_auth:OAuth bearer(若用 OAuth)或 API key 注入;
  - rewrite_transport:**多 URL 池可用性管理**——daily / prod / sandbox 多个 base URL,按可用性轮询;失败的 URL 进入 TTL 冷却,自动恢复;
  - patch_request:无;
  - patch_response:无;
  - transport_profile:无。
- **不在范围**：
  - thinking signature cache(已经在桶 1 codec S1 IR 里建模,不归 adapter);
  - Antigravity 协议形态(它是 Gemini 兼容 + 一些扩展,codec 走 Gemini codec);
  - 可用性探测的"主动 probe"(本 spec 只做"被动失败检测",主动探测放 K-D)。

> A-T4 是 6 个 vendor 里最薄的,主要价值在于 URL 可用性池管理。如果 Antigravity 长期稳定,可以撤销本 spec。

---

## 1. 背景

Antigravity 提供多个 base URL(daily / prod / sandbox),每个有不同限流策略与稳定性。Aether `url_availability.py` 的逻辑:

1. 进程内维护 unavailable URL 集合;
2. 收到 5xx / 网络错误 → 标记 URL unavailable + 设置恢复时间(默认 60s);
3. 选 URL 时:跳过 unavailable;优先用 "最近成功的"(亲和);
4. 全部 unavailable 时仍按 base_order 重试(让 TTL 自动恢复)。

本 spec 在 Rust 落地这套机制。注意:这与 K-A 的 KeyHealth 是**不同维度**——K-A 是 vendor_key 级别的健康度,A-T4 是同一 vendor 下多个 base URL 的健康度。一个 antigravity vendor_key 可能在 daily URL 上 503 但 prod URL 正常,需要 URL 级别的健康跟踪。

## 2. 设计决策

### ADR-AT4-1 vendor 注册

```
vendor_id = "antigravity"
```

### ADR-AT4-2 URL 池数据结构

```rust
// crates/gw-adapter/src/antigravity/url_pool.rs

pub struct UrlPool {
    /// vendor-level shared,所有 antigravity vendor_key 共用同一个 pool
    /// (因为 base URL 是 vendor 全局属性,不是 per-key 配置)。
    inner: Arc<UrlPoolInner>,
}

struct UrlPoolInner {
    /// URL 列表(daily / prod / sandbox),顺序即默认优先级。
    base_order: Vec<String>,
    /// (url) → 不可用恢复时间(UNIX ms);用 DashMap 写入并发。
    unavailable: DashMap<String, u64>,
    /// 最近成功的 URL(亲和优化)。
    last_success: ArcSwap<Option<String>>,
}

impl UrlPool {
    pub fn pick(&self) -> String {
        let now_ms = now_ms();
        // 1. 清理已过期 unavailable entry
        self.inner.unavailable.retain(|_url, recover_at| *recover_at > now_ms);

        // 2. 找 last_success(若仍可用)
        if let Some(url) = self.inner.last_success.load().as_ref() {
            if !self.inner.unavailable.contains_key(url.as_str()) {
                return url.clone();
            }
        }

        // 3. 按 base_order 找第一个可用
        for url in &self.inner.base_order {
            if !self.inner.unavailable.contains_key(url) {
                return url.clone();
            }
        }

        // 4. 全部 unavailable,返回首个(强制重试,等 TTL 自动恢复)
        self.inner.base_order[0].clone()
    }

    pub fn mark_unavailable(&self, url: &str, ttl_secs: u32) {
        let recover_at = now_ms() + (ttl_secs as u64) * 1000;
        self.inner.unavailable.insert(url.to_owned(), recover_at);
        // 若 last_success == url,清掉(强制下次重选)
        let current = self.inner.last_success.load();
        if current.as_deref() == Some(url) {
            self.inner.last_success.store(Arc::new(None));
        }
    }

    pub fn mark_success(&self, url: &str) {
        self.inner.last_success.store(Arc::new(Some(url.to_owned())));
    }
}
```

### ADR-AT4-3 constants

```rust
// crates/gw-adapter/src/antigravity/constants.rs

pub const DAILY_BASE_URL: &str = "https://daily.<antigravity-host>";  // 实际 URL 由部署确认
pub const PROD_BASE_URL: &str = "https://prod.<antigravity-host>";
pub const SANDBOX_BASE_URL: &str = "https://sandbox.<antigravity-host>";

pub const URL_UNAVAILABLE_TTL_SECONDS: u32 = 60;

pub fn default_base_order() -> Vec<String> {
    vec![
        DAILY_BASE_URL.to_owned(),
        PROD_BASE_URL.to_owned(),
        SANDBOX_BASE_URL.to_owned(),
    ]
}
```

⚠️ 实际 URL 值需要从 Aether 仓库 source 同步(本 spec 略,实施时确认)。

### ADR-AT4-4 AntigravityAdapter

```rust
// crates/gw-adapter/src/antigravity/adapter.rs

pub struct AntigravityAdapter {
    pool: UrlPool,
}

impl AntigravityAdapter {
    pub fn new() -> Self {
        Self {
            pool: UrlPool {
                inner: Arc::new(UrlPoolInner {
                    base_order: constants::default_base_order(),
                    unavailable: DashMap::new(),
                    last_success: ArcSwap::new(Arc::new(None)),
                }),
            },
        }
    }

    pub fn url_pool(&self) -> &UrlPool { &self.pool }
}

#[async_trait]
impl UpstreamAdapter for AntigravityAdapter {
    fn name(&self) -> &'static str { "antigravity" }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        // 标准 API key:仅注 x-api-key 头(或 Authorization,具体看 antigravity 协议)
        let key = ctx.vendor_key.api_key.expose_secret();
        ctx.headers_mut.set("x-api-key", key);
        ctx.headers_mut.remove("authorization");
        Ok(())
    }

    fn rewrite_transport(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let url = self.pool.pick();
        // 把 ctx.url 的 host 与 scheme 改为选中的 base URL
        let parsed = url::Url::parse(&url)
            .map_err(|e| AdapterError::Transport(format!("antigravity base URL invalid: {e}")))?;
        ctx.url_mut.set_scheme(parsed.scheme());
        ctx.url_mut.set_host(parsed.host_str().unwrap_or(""));
        // path 由客户端请求决定(已经在 ctx.url_mut 里),不动 path
        // ※ 把选中的 URL 写到 ctx 上让 response_filter 能 mark_success / mark_unavailable
        ctx.metadata.insert("antigravity.url".into(), url);
        Ok(())
    }

    // patch_request / patch_response no-op
}
```

`ctx.metadata: HashMap<String, String>` 是 AdapterCtx v2 新增的"adapter 内部传递信息"槽位——这是与 trait v2 的小修订,加进 A-T0 spec(把 metadata 字段加进 AdapterCtx)。

### ADR-AT4-5 与 K-B 的协作

URL pool 的失败标记由 K-B error_analyzer 触发:

```rust
// crates/gw-adapter/src/antigravity/error_analyzer.rs

pub struct AntigravityErrorAnalyzer {
    pool: UrlPool,  // 共享同一个 pool
}

impl UpstreamErrorAnalyzer for AntigravityErrorAnalyzer {
    fn analyze(&self, ctx: &ErrorAnalyzeCtx<'_>) -> ErrorOutcome {
        // 网关错误回路上拿到 ctx.headers 没有"我们用了哪个 URL",
        // 但 ctx 应当包含 metadata("antigravity.url")。这需要 ErrorAnalyzeCtx 扩字段,
        // 让 adapter 在 rewrite_transport 写的 metadata 透到 error_analyzer。
        let url = ctx.adapter_metadata.get("antigravity.url");

        match ctx.http_status {
            500..=599 | 502 | 503 | 504 => {
                if let Some(u) = url {
                    self.pool.mark_unavailable(u, constants::URL_UNAVAILABLE_TTL_SECONDS);
                }
                // 不冷却 key——只是这个 URL 不可用,key 仍然好的
                ErrorOutcome {
                    cooldown: None,
                    oauth_signal: None,
                    count_as_failure: false,
                }
            }
            _ => DefaultErrorAnalyzer.analyze(ctx),
        }
    }
}
```

成功路径同理:`response_filter` 看到 2xx → 调 `pool.mark_success(url)`。

⚠️ 这需要 K-B 的 `ErrorAnalyzeCtx` 加 `adapter_metadata: &HashMap<String, String>` 字段——这是 K-B 的小修订,加到 K-B spec follow-up 里。

### ADR-AT4-6 与 K-D 的关系

K-D 提供"主动探测"框架,但 antigravity 没有 dashboard quota——本 adapter 的"主动健康探测"不通过 K-D,因为 K-D 是 quota probe 而非 endpoint health probe。

若想加 endpoint health probe(主动 HEAD 各 base URL),可以:

- 起一个独立后台 tokio task(在 antigravity adapter 内部);
- 周期 HEAD 各 base URL → 失败时调 pool.mark_unavailable;
- v1 不做(被动检测够用)。

### ADR-AT4-7 metadata 字段(trait v2 微调)

A-T0 ADR-AT0-2 的 AdapterCtx 需要加:

```rust
pub struct AdapterCtx<'a> {
    // ...原字段
    /// adapter 内部传递信息(rewrite_transport 写,后续阶段读)
    pub metadata: HashMap<String, String>,
}
```

K-B 的 ErrorAnalyzeCtx 同样加:

```rust
pub struct ErrorAnalyzeCtx<'a> {
    // ...原字段
    /// adapter 在 rewrite_transport 阶段写入的 metadata(error 路径读)
    pub adapter_metadata: &'a HashMap<String, String>,
}
```

加这两个字段把"adapter 多阶段传值"统一起来,不只是 antigravity 用,A-T3 vertex_ai 可以传 selected region,A-T5 codex 可以传 transformed body flag。

### ADR-AT4-8 测试

- UrlPool::pick 全部可用 → 返 base_order[0] 或 last_success;
- pool.mark_unavailable → pick 跳过该 URL;
- 全部 unavailable → 仍返 base_order[0](允许重试);
- TTL 过期 → 自动可用;
- error_analyzer 5xx → mark_unavailable(同时不冷却 key);
- 并发:多线程同时 pick / mark_unavailable 不 race。

## 3. 实施大纲

依赖:A-T0 land + K-A/K-B land + `arc-swap` 已存在或新增。

1. `crates/gw-adapter/src/antigravity/` 模块;
2. constants.rs / url_pool.rs / adapter.rs / error_analyzer.rs;
3. A-T0 trait v2 spec 微调:AdapterCtx 加 metadata 字段(同 PR);
4. K-B spec 微调:ErrorAnalyzeCtx 加 adapter_metadata 字段;
5. 单测覆盖。

## 4. 未决问题

1. **antigravity 真实 base URL**: Aether 代码里 host 不公开,需要实施时同步。
2. **URL pool 是 per-vendor 还是 per-vendor-key**: 倾向 per-vendor(antigravity 全局共享一个 pool),与 Aether 一致。
3. **跨副本共享**: 多副本 Rust 网关各自维护 UrlPool。副本 A 标 URL_X 不可用,副本 B 不知道——B 仍可能选 URL_X。代价是首次 retry 在 B 上失败,B 再各自学一次。可接受(v1 KeyHealth 也是同款)。后续可考虑用 Redis SET 同步,但不在 v1。
4. **主动 health probe**: v1 不做。若上游波动严重,后续起独立 task 即可,与 K-D scheduler 同款架构。
5. **mark_success 时机**: response_filter 看到 2xx 就 mark?或者要看完整 body 才算?倾向 status 2xx + 收到第一个 body chunk = 算成功(避免响应中途断流被误标)。具体逻辑放 gw-proxy 接入时定。
6. **failing URL 的 ttl 是否分级**: 503(瞬时) vs 502(可能配置问题) → 同 60s?Aether 是一刀切。倾向 v1 沿用,后续按 K-B 给的 ttl 不同分级。
7. **AdapterCtx.metadata 类型选 HashMap 还是更轻量结构**: 业务 metadata 通常 1-3 项,HashMap 分配开销略大(~32 bytes vec)。可以用 `SmallVec<[(String, String); 4]>` 或带 const N 的 fixed array。倾向 v1 用 HashMap(简单),后续 benches 决定是否优化。

---

> A-T4 是 6 个 vendor 里写得最薄的,但暴露了 trait v2 的两个微调点(metadata 字段、ErrorAnalyzeCtx adapter_metadata),这两个微调推到 A-T0 / K-B spec 同 PR 实施。
