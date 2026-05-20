# A-T5 — codex Adapter(v2026-05-19)

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-adapter-a-t0-trait-v2.md](./2026-05-19-adapter-a-t0-trait-v2.md)(**硬前置**)
  - [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md)(OpenAI Responses 形态,可能涉及 C8 新 codec)
  - [2026-05-19-codec-s3-sse-multi-event.md](./2026-05-19-codec-s3-sse-multi-event.md)(SSE 多事件状态机)
  - Aether 来源:`src/services/provider/adapters/codex/`
- **范围**：Codex 反代上游 adapter:
  - rewrite_auth:OAuth bearer(ChatGPT 账号 OAuth);
  - rewrite_transport:URL → `/responses`(Codex 唯一 endpoint);
  - patch_request:**图像生成请求包装** —— OpenAI `/v1/images/generations` 请求 → Codex `/responses` + image_generation tool 调用;
  - patch_response:**SSE 事件聚合** —— Codex 的 `response.*` 事件流 → 标准 OpenAI images JSON / SSE;
  - transport_profile:无。
- **不在范围**：
  - OpenAI Responses 形态本身(→ codec C8 spec,这是新 codec);
  - codex quota 解析(→ K-D 的 codex_quota extractor);
  - codex compact 请求(`is_codex_compact_request` 检测逻辑,Aether 内部用,Rust v1 不实现)。

> A-T5 是 6 个 vendor 里**最重**的——涉及 wire 上的实质 body 改写 + SSE 事件流改写。它也是验证 trait v2 `patch_request/patch_response` 表达力的关键 case。

---

## 1. 背景

Codex 反代是 ChatGPT/Codex 桌面端用的内部 endpoint(`chatgpt.com/backend-api/codex`),它**不**暴露标准 OpenAI 接口:

- 没有 `/v1/chat/completions`(标准 chat 仍走 OAuth Bearer +;但只有 `/responses`);
- **完全没有** `/v1/images/generations` —— 图像生成被包装在 `/responses` + image_generation tool 调用;
- 流式响应是 OpenAI Responses 格式的 SSE(`response.created` / `response.output_item.done` / `response.completed` 等),不是标准 chat completion 的 `data: {...}` delta。

Aether 实现两道转换:

1. **请求侧**(`image_transform.py:transform_image_request_to_responses`):
   - OpenAI image request: `{model, prompt, n, size, ...}` 
   - → Codex Responses payload:
     ```json
     {
       "model": "gpt-5.4-mini",
       "input": [{"type":"message","role":"user","content":[...]}],
       "instructions": "",
       "tool_choice": {"type":"image_generation"},
       "tools": [{"type":"image_generation", "model": "<真实图像模型>", "size":"...", ...}],
       "reasoning": {...},
       "parallel_tool_calls": false,
       "include": [...]
     }
     ```
2. **响应侧**(`sse_events.py:parse_codex_sse_payload` + `image_transform.py:aggregate_codex_image_sse`):
   - 收 SSE `response.output_item.done` 事件(item.type=image_generation_call)→ 提取 base64 图像;
   - 收 `response.completed` → 提取 usage;
   - 聚合后输出标准 OpenAI images JSON `{data: [{b64_json: ...}], usage: {...}}` 或合成一个 `image_generation.completed` SSE 帧(stream=true 时)。

## 2. 设计决策

### ADR-AT5-1 vendor 注册

```
vendor_id = "codex"
```

### ADR-AT5-2 形态决策:走 patch_request/patch_response 还是新 codec?

两条路:

| 路径 | 实现位置 | 优缺 |
|---|---|---|
| (A) 在 adapter `patch_request/patch_response` 改 wire | gw-adapter | 复用现 OpenAI wire codec;adapter 跨多个 wire 改写复杂(请求是 OpenAI images,目标是 OpenAI Responses,两种 wire 都需要在矩阵里) |
| (B) 新增 CodexResponses codec(对应 OpenAI Responses 形态) | gw-codec | 干净(codex 是真实的 wire 不同),但工作量大——OpenAI Responses 是 wire enum 矩阵的新元素(N=4 → 16 个直转) |

**v1 选 (A)**:
- 因为 Codex 是"非标准 endpoint",独立 codec 投入产出比低(仅服务于 codex vendor);
- patch_request/patch_response 钩子刚好为这种 case 设计;
- 代价是 adapter 内部要持有"OpenAI images request → Codex Responses request"的转换函数(私有 helper),不进 codec 矩阵。

未决:若以后 OpenAI Responses 成为标准 chat 形态(OpenAI 自己也支持 chat-completions 之外的 /responses),C8 spec 独立做新 codec,A-T5 改走 (B)。

### ADR-AT5-3 constants

```rust
// crates/gw-adapter/src/codex/constants.rs

pub const CODEX_BASE_URL: &str = "https://chatgpt.com";
pub const CODEX_API_PATH: &str = "/backend-api/codex/responses";

/// Codex 内部图像路由模型;所有 image 请求 outer model 都用它。
pub const IMAGE_ROUTING_MODEL: &str = "gpt-5.4-mini";

pub const STAINLESS_HEADERS: &[(&str, &str)] = &[
    ("X-Stainless-Lang", "js"),
    ("X-Stainless-Package-Version", "..."),  // 需要从 Aether 同步真实值
    // 与 claude_code 同款,但 sdk 版本不同
];
```

### ADR-AT5-4 patch_request:image generation wrapping

```rust
// crates/gw-adapter/src/codex/image_transform.rs

pub fn transform_image_to_responses(
    user_model: &str,           // 用户真实图像模型,如 "gpt-image-2"
    prompt: &str,
    image_params: &ImageParams, // n / size / quality / 等
    mask: Option<&[u8]>,        // edit endpoint 的 mask 数据
) -> Result<CodexResponsesBody, AdapterError> {
    if image_params.n > 1 {
        return Err(AdapterError::PatchRequest("Codex image tool does not support n>1".into()));
    }

    let mut tool = serde_json::Map::new();
    tool.insert("type".into(), "image_generation".into());
    tool.insert("model".into(), user_model.into());
    // 透传字段 size / quality / background / output_format / ...
    for &k in &TOOL_PASSTHROUGH_KEYS {
        if let Some(v) = image_params.get(k) {
            tool.insert(k.into(), v.clone());
        }
    }
    if let Some(m) = mask {
        let b64 = base64_engine.encode(m);
        tool.insert(
            "input_image_mask".into(),
            json!({"image_url": format!("data:image/png;base64,{}", b64)}),
        );
    }

    let body = json!({
        "model": IMAGE_ROUTING_MODEL,
        "input": [{"type":"message","role":"user","content":[{"type":"input_text","text": prompt}]}],
        "instructions": "",
        "tool_choice": {"type":"image_generation"},
        "tools": [tool],
        "reasoning": {"effort": "medium"},
        "parallel_tool_calls": false,
        "include": ["reasoning.encrypted_content","image_generation_call.result"],
    });
    Ok(CodexResponsesBody(body))
}
```

`patch_request` 钩子里调:

```rust
fn patch_request(&self, ctx: &AdapterCtx<'_>, wire: &mut WireAnyRequest<'_>) -> Result<(), AdapterError> {
    // 检测是否为 image 请求(基于 ctx.metadata 或 wire path)
    if !is_image_request(ctx) {
        return Ok(());  // 非 image 请求:直接透传(普通 chat 走 OAuth + /responses)
    }

    // 1. 从 wire(WireAnyRequest::OpenAi)提取 prompt + image params
    let WireAnyRequest::OpenAi(openai) = wire else {
        return Err(AdapterError::PatchRequest("Codex image requires OpenAI client protocol".into()));
    };
    let (prompt, model, params) = extract_image_request_fields(openai)?;

    // 2. 转换成 Codex Responses body(serde_json::Value)
    let codex_body = transform_image_to_responses(&model, &prompt, &params, /* mask=*/ None)?;

    // 3. 替换 wire 内部 body —— 这是关键:wire 的 body 字段被整体替换
    //    问题:WireAnyRequest 是 codec 已知 wire 类型,无法直接塞任意 JSON。
    //    解决:wire 加一个"opaque body" 兜底变体 OR 在 ctx 上挂一个 "override_body"
    //    倾向后者:ctx.metadata.insert("codex.override_body", serde_json::to_string(&codex_body)?);
    //    然后 gw-proxy 的 upstream_request_filter 在发送 wire 时优先看 override_body 字段
    ctx.metadata.insert("codex.override_body".to_owned(), codex_body.to_string());
    Ok(())
}
```

⚠️ **关键问题**:在 codec v2 模型下,wire 是强类型 enum。Codex 重写 body 不是改 wire 字段,而是**整个换掉**——这与 WireAny 的强类型矛盾。两种解决方案:

| 方案 | 实现 | 代价 |
|---|---|---|
| (P1) wire 加 `Raw` 变体: `WireAnyRequest::Raw(Box<RawValue>)` | wire enum 加一个 untyped 旁路 | 污染 wire 抽象 |
| (P2) ctx.metadata 带 `override_body` 字符串,gw-proxy 优先用 | 不改 wire,在传输层兜底 | adapter 与 gw-proxy 紧耦合(adapter 知道 gw-proxy 行为) |

倾向 (P2):wire enum 保持纯净,override_body 是 codex 特有的"绕路口",不污染通用模型。gw-proxy `upstream_request_filter` 在发送前先读 `ctx.metadata.get("override_body")`,有则用,否则 codec render wire 出 bytes。

### ADR-AT5-5 patch_response:Codex SSE → OpenAI images

```rust
// crates/gw-adapter/src/codex/sse_aggregator.rs

pub struct CodexImageStreamState {
    pub created_at: Option<u64>,
    pub b64_image: Option<String>,
    pub usage: Option<UsageSnapshot>,
    pub error: Option<CodexStreamError>,
}

pub fn parse_codex_sse_payload(payload: &str, state: &mut CodexImageStreamState)
    -> Result<(), CodexStreamError>
{
    let event: serde_json::Value = serde_json::from_str(payload).map_err(...)?;
    let evt_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match evt_type {
        "response.created" => {
            state.created_at = event.get("response").and_then(|r| r.get("created_at")).and_then(|v| v.as_u64());
        }
        "response.output_item.done" => {
            if let Some(item) = event.get("item") {
                if item.get("type").and_then(|v| v.as_str()) == Some("image_generation_call") {
                    state.b64_image = item.get("result").and_then(|v| v.as_str()).map(|s| s.to_owned());
                }
            }
        }
        "response.completed" => {
            state.usage = extract_codex_image_usage(&event);
        }
        "response.failed" | "response.incomplete" | "response.error" => {
            return Err(build_stream_error(evt_type, &event));
        }
        _ => {
            tracing::debug!(event = %evt_type, "codex SSE event ignored");
        }
    }
    Ok(())
}
```

`patch_response` 钩子在 SSE 终态触发:

```rust
fn patch_response(&self, ctx: &AdapterCtx<'_>, wire: &mut WireAnyResponse<'_>) -> Result<(), AdapterError> {
    if !is_image_request(ctx) { return Ok(()); }

    // 流式 vs 非流不同处理。
    // 非流:聚合后构造 OpenAI images JSON 替换响应 body
    // 流:在客户端流上合成一个 image_generation.completed 帧
    //
    // 这里复用 patch_request 同款的"override_body"机制——adapter 把聚合结果写
    // 到 ctx.metadata("codex.override_response_body"),由 gw-proxy 发送给客户端。
    let state = ctx.metadata.get("codex.stream_state")
        .and_then(|s| serde_json::from_str::<CodexImageStreamState>(s).ok())
        .unwrap_or_default();

    if let Some(err) = state.error {
        return Err(AdapterError::PatchResponse(format!("codex upstream error: {err}")));
    }
    let openai_images_body = build_openai_images_response(&state)?;
    ctx.metadata.insert("codex.override_response_body".into(), openai_images_body.to_string());
    Ok(())
}
```

⚠️ **stream_state 跨 chunk 累积**:codec S3 的 framer 在 gw-proxy 层,chunk 来一个 codec parse 一个。codex SSE 累积状态(b64_image 拼接) 与 codec S3 的 `WireStreamTarget::Accum` 概念冲突——
- codec S3 累积是 wire→wire 直转用的(thinking signature 拼);
- codex SSE 累积是 vendor 内部用的(image base64 在某帧到达,而非跨帧拼).

倾向:codex 的累积状态由 adapter 持有(放 `AdapterCtx::metadata`),不进 codec 累积机制。每个 chunk 到达,adapter 在 patch_response 里 parse → 更新 state;终态时 build OpenAI images response。

### ADR-AT5-6 CodexAdapter 实现

```rust
pub struct CodexAdapter {
    refresher: Arc<dyn OAuthRefresher>,
}

#[async_trait]
impl UpstreamAdapter for CodexAdapter {
    fn name(&self) -> &'static str { "codex" }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let token = ctx.oauth_cache.get_access_token(&ctx.vendor_key, &*self.refresher).await?;
        ctx.headers_mut.set("authorization", &format!("Bearer {}", token.expose_secret()));
        ctx.headers_mut.remove("x-api-key");
        for (k, v) in constants::STAINLESS_HEADERS {
            ctx.headers_mut.set(k, v);
        }
        Ok(())
    }

    fn rewrite_transport(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        ctx.url_mut.set_scheme("https");
        ctx.url_mut.set_host("chatgpt.com");
        ctx.url_mut.set_path(constants::CODEX_API_PATH);
        Ok(())
    }

    fn patch_request(&self, ctx: &AdapterCtx<'_>, wire: &mut WireAnyRequest<'_>)
        -> Result<(), AdapterError>
    {
        // 见 §ADR-AT5-4
    }

    fn patch_response(&self, ctx: &AdapterCtx<'_>, wire: &mut WireAnyResponse<'_>)
        -> Result<(), AdapterError>
    {
        // 见 §ADR-AT5-5
    }
}
```

### ADR-AT5-7 K-B / K-C / K-D

- **K-C OAuth refresher**: Codex 的 token endpoint 是 chatgpt.com OAuth(refresh_token grant)。实现类同 A-T1 / A-T2。
- **K-B error analyzer**: codex 错误 body 含 `request_id`(给用户去 OpenAI help center 查),analyzer 应在错误响应里把 request_id 提到 `ErrorOutcome` 让 logging 记录。403 SERVICE_DISABLED → AccountBlocked(账号被封)。
- **K-D quota**: Codex 响应头有 `x-codex-quota-remaining`(参 K-D ADR-KD-3),`CodexQuotaExtractor` 在 K-D 已规划。

### ADR-AT5-8 测试

- transform_image_to_responses:OpenAI images → Codex body 形态正确;
- transform_image_to_responses:n > 1 → 400;
- parse_codex_sse_payload:response.output_item.done → b64 提取;
- parse_codex_sse_payload:response.failed → CodexStreamError;
- patch_request 非 image 请求 → no-op(普通 chat 走标准 /responses);
- patch_response 聚合 SSE → OpenAI images JSON;
- 错误 analyzer 提取 request_id 到 error_code field。

## 3. 实施大纲

依赖:A-T0 + K-A/K-B/K-C/K-D land + codec S3(stream framer)。

1. `crates/gw-adapter/src/codex/` 模块;
2. constants.rs / image_transform.rs / sse_events.rs / adapter.rs / refresher.rs / error_analyzer.rs;
3. `AdapterCtx::metadata` "override_body" / "override_response_body" 协议 + gw-proxy 接入;
4. 单测覆盖。

## 4. 未决问题

1. **override_body 机制是否进 A-T0 spec**: 这是 A-T5 暴露的需求(任意 vendor 改写 body 都可能用)。倾向加进 A-T0 v2,作为正式钩子;A-T5 spec 引用即可。
2. **CodexImageStreamState 跨 chunk 持有方式**: ctx.metadata 是 `HashMap<String, String>`,序列化 state 进 String 再 parse 出来效率低。可能要给 ctx 加 `state: Arc<DashMap<String, Box<dyn Any>>>` 让 adapter 持原生 state。复杂度上升,留 follow-up。
3. **stream 与非流响应路径分歧**: codex 总是 stream(SSE),但客户端可能要非流 OpenAI images JSON。adapter 在 patch_response 里要把 stream 聚合为非流——但 patch_response 调用时机是"收完整响应 body 后",还是"每个 chunk 后"?需要在 trait v2 区分两种回调。倾向加 `patch_response_chunk` 与 `patch_response_final` 两个钩子,各自语义清晰。但本 spec 范围内仅描述需求,实施推到 v2。
4. **non-image 请求的处理**: codex 也支持普通 chat。普通 chat 也走 /responses 但响应不是 image,而是 OpenAI Responses 格式 SSE。codex 客户端如果不发 image 请求,adapter 完全 no-op?但 OpenAI Responses 与标准 chat-completions 不同,客户端可能预期 chat 格式响应——需要 Responses → chat-completions 的转换。这又是个 codec 工作。**v1 codex adapter 仅支持 image 用例**;chat 用例(C8 spec)独立做。
5. **image_generation_call.result 累积**: b64 数据完整在一个 SSE event 里(`output_item.done`),不是跨 chunk 拼接(与 thinking signature 不同)。所以累积状态比较简单,主要是等齐 created + done + completed 三个事件。
6. **codex 的 image edits / multipart**: Aether 还实现了 `/v1/images/edits` 的 multipart parsing。v1 倾向先做 `/v1/images/generations`,edits 推 follow-up。
7. **OPENAI_IMAGE_ROUTING_MODEL 同步**: 这个常量(`"gpt-5.4-mini"`)随 Codex 升级会变。需要纳入 entity 配置,而不是 const。倾向 v1 const + monitor;若发现变动频繁推 entity。

---

> A-T5 是 6 个 vendor 里最复杂的——也是 trait v2 patch_request/patch_response 钩子表达力的真正验证。完成后,其他 vendor 出现类似 wrapping 需求(如未来某 vendor 把 chat 包装成 batch)都按本 spec 同款套路。
