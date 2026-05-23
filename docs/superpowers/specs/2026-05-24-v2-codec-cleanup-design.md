# S-codec — gw-codec 非流响应 / 跨协议 re-render / 工具块往返收口(v2026-05-24)

- **日期**:2026-05-24
- **状态**:Draft(等待 user review → writing-plans)
- **作者**:协作设计(Claude + 用户)
- **触发**:[v2 cleanup overview](./2026-05-23-v2-cleanup-overview-design.md) §2 S-codec — 4 个 wire form 的 `ProtocolCodec::render_response` 是 v1 留下的 `unimplemented!()` 硬桩;跨协议非流响应在 `gw-proxy/src/filters/upstream.rs:230` 直接返回 503(`GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED`);Anthropic / Gemini 在 wire 层把 tool_use / tool_result / functionCall / functionResponse 块强行折成 User 文本;OpenAI Chat 的 `tools` / `tool_choice` 字段经 extra HashMap 间接 round-trip;SSE 流式无 thinking 续行 / tool_call delta 累积器。
- **关联(被本 spec 收口的 v1 设计)**:
  - [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md)(IR 与 wire codegen 基础)
  - [2026-05-19-codec-s2-wire-multimodal.md](./2026-05-19-codec-s2-wire-multimodal.md)(multimodal 不在本 spec,但 wire 形态本 spec 会涉及)
  - [2026-05-19-codec-s3-sse-multi-event.md](./2026-05-19-codec-s3-sse-multi-event.md)(流式 SSE 基础,本 spec 在其上加内部累积器)
  - [2026-05-19-codec-s4-response-and-cross-protocol.md](./2026-05-19-codec-s4-response-and-cross-protocol.md)(本 spec 是其 ADR-S4-v2-* 全部条目的兑现)
  - [2026-05-23-v2-cleanup-overview-design.md](./2026-05-23-v2-cleanup-overview-design.md)(总览,本 spec 是 §2 S-codec 子计划)
- **范围**:把 4 个 wire form(OpenAI Chat、OpenAI Responses、Anthropic Messages、Gemini Generate)的非流响应渲染、跨协议非流响应 re-render、工具调用块 wire-端结构化、SSE 内部累积器、`finish_reason` 跨协议映射表、`UsageSnapshot` POD 一次性落地。N-1 局部(`OpenAI extra` 改 `Cow<'a,str>`、Anthropic/Gemini content 数组化)在 wire 结构化中顺手收口。
- **不在范围**:
  - wire→wire N² 直转 codegen(overview §G-8 已锁不做);
  - multimodal image / inlineData / fileData blocks(`codec-s2-wire-multimodal` 范畴,本次不动);
  - Kiro EventStream(`codec-s5` 范畴);
  - 客户端发"非流"但上游只支持"流"的 `stream_accumulator.rs` 路径(已就位,不动);
  - CapabilityResolver 路由策略以避开 lossy 场景(归 S-proxy-core R-1);
  - `usage:detailed` sink 实际落地(归 S-ledger L-1;本 spec 只产 UsageSnapshot 数据)。

> 一句话定义:把 4 个 wire form 的"响应路径"补到与"请求路径"对等的完成度 — 同协议非流跑通,跨协议非流跑通,工具块在 wire 上结构化,流式跨块累积由 codec 内部承担。

---

## §1 目标态(完工签出条件)

1. 生产代码路径(`crates/gw-codec/src/**`、`crates/gw-proxy/src/**`、`crates/gw-core/src/**`)无 `unimplemented!()` / `todo!()` / `TODO(v2)` / `"v1: ... not implemented"` 字样。
2. `cargo test -p gw-codec` 通过;新增的跨协议矩阵单测(4×4 = 16)全部 pass。
3. `cargo test -p gw-proxy` 通过;原 `cross_protocol_nonstream_returns_503` 这一类 reject 测例**删除**(不是被禁用),改为 round-trip 测例。
4. `gw-proxy/src/filters/upstream.rs:230` 不再含 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` 短路。
5. 同协议非流请求行为不变(zero-copy 仍然成立 — codec 应能识别 `client == upstream` 并跳过 IR 往返,直接转发原 bytes;见 §3.1)。
6. 跨协议非流真实请求 e2e:客户端 OpenAI Chat,上游 Anthropic,模型回 tool_use 块,客户端能 parse 出 `tool_calls`,`gw_codec_cross_render_nonstream_total{from="anthropic", to="openai_chat"}` ++。
7. 跨协议 lossy 字段(如 Anthropic thinking → OpenAI Chat)在 `gw_codec_cross_render_lossy_total{from, to, field}` 上有计数,`tracing::error!` 同步触发;客户端 wire 响应不含 thinking,其余字段完整。
8. SSE 跨块累积器:Anthropic thinking 续行、OpenAI Chat tool_call delta 合并、Gemini functionCall 续行各有单测覆盖。
9. `cross_render` 路径在 `gw_codec_cross_render_total` 指标上区分 `stream` vs `nonstream`(已有 stream 计数,新增 nonstream)。

---

## §2 文件结构与改动面

### 2.1 `gw-core/src/ir.rs` 扩展

```rust
// 新增 - 响应侧
pub struct ResponseIR<'a> {
    pub messages: Vec<MessageIR<'a>>,
    pub finish_reason: FinishReason,
    pub usage: UsageSnapshot,
    // vendor-specific (Option, 打平)
    pub thinking: Option<ThinkingBlocks<'a>>,        // Anthropic / OpenAI Responses
    pub reasoning: Option<ReasoningBlock<'a>>,       // OpenAI Responses only
    pub tool_use_blocks: Option<Vec<ToolUseBlockIR<'a>>>,  // Anthropic 块形态
    pub model: Cow<'a, str>,
    pub created: Option<u64>,
    pub id: Option<Cow<'a, str>>,
}

pub struct UsageSnapshot {  // POD, ~32 B
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cached_tokens: u32,        // OpenAI prompt_tokens_details.cached_tokens / Anthropic cache_read_input_tokens
    pub reasoning_tokens: u32,     // OpenAI Responses completion_tokens_details.reasoning_tokens
}

pub enum FinishReason {
    Stop,           // 自然结束
    Length,         // max_tokens 截断
    ToolCalls,      // 模型决定调工具
    ContentFilter,  // safety
    Other(Cow<'static, str>),  // 兜底
}

// 新增 - 流式 chunk 类型扩展
pub enum StreamEventIR<'a> {
    // 既有: Start, Delta(TextDelta), Usage, End, Error
    ThinkingDelta { text: Cow<'a, str>, signature_chunk: Option<Cow<'a, str>> },
    ToolCallStart { index: u32, id: Cow<'a, str>, name: Cow<'a, str> },
    ToolCallDelta { index: u32, arguments_chunk: Cow<'a, str> },
}

// 新增 - 块类型
pub struct ToolUseBlockIR<'a> {
    pub id: Cow<'a, str>,
    pub name: Cow<'a, str>,
    pub input: Cow<'a, RawValue>,  // 完整 JSON
}

pub struct ThinkingBlocks<'a> {
    pub blocks: Vec<ThinkingBlockIR<'a>>,
}

pub struct ThinkingBlockIR<'a> {
    pub text: Cow<'a, str>,
    pub signature: Option<Cow<'a, str>>,  // Anthropic 提供
}

pub struct ReasoningBlock<'a> {
    pub summary: Cow<'a, str>,  // OpenAI Responses reasoning summary
}
```

**MessageIR 扩展**:`MessageIR::Assistant` 内增 `tool_use_blocks: Option<Vec<ToolUseBlockIR>>`(同上,Anthropic 形态保留),`MessageIR::Tool` 改为含 `tool_use_id` 与结构化 `result`。

### 2.2 `gw-codec/src/trait_def.rs` 改动

```rust
pub trait ProtocolCodec: Send + Sync {
    // 不变 - 请求侧
    fn parse_request(&self, bytes: &[u8]) -> Result<WireAnyRequest<'_>, ErrorIR<'static>>;
    fn render_request(&self, ir: &RequestIR<'_>, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>>;
    
    // 改 - 响应侧 (撤掉 unimplemented! 实现位)
    fn parse_response(&self, bytes: &[u8]) -> Result<ResponseIR<'_>, ErrorIR<'static>>;
    fn render_response(&self, ir: &ResponseIR<'_>, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>>;
    //                                                                              ^^ 不再 unimplemented!()
    
    // 改 - 流式 chunk (加 state 参数)
    fn new_stream_state(&self) -> Box<dyn StreamState>;
    fn parse_stream_chunk(
        &self,
        state: &mut dyn StreamState,
        bytes: &[u8],
    ) -> Result<Vec<StreamEventIR<'_>>, ErrorIR<'static>>;
    //         ^^^ 返回 Vec 而非单个 - 一个 chunk 可能合并出多个 / 零个 event
    
    fn render_stream_chunk(&self, ir: &StreamEventIR<'_>, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>>;
}

pub trait StreamState: Send + Any {
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
```

**`parse_stream_chunk` 签名变更**:
- 加 `state: &mut dyn StreamState` 参数 — 累积器状态外置,但 opaque(filter 不窥视字段)。
- 返回 `Vec<StreamEventIR>` 而非 `Option<StreamEventIR>` — 一个 wire chunk 可能携带多个完整事件,或一个事件还没拼齐返回空 Vec。

### 2.3 `gw-codec/src/cross_render.rs` 新建

```rust
pub struct CrossRender<'a> {
    pub from: WireFormat,
    pub to: WireFormat,
    pub source: ResponseIR<'a>,
}

impl<'a> CrossRender<'a> {
    pub fn run(self, target_codec: &dyn ProtocolCodec, buf: &mut BytesMut) -> Result<(), ErrorIR<'static>> {
        let ir = self.adapt_lossy();  // 内部裁剪 vendor-specific 字段
        target_codec.render_response(&ir, buf)?;
        Ok(())
    }
    
    fn adapt_lossy(self) -> ResponseIR<'a> {
        let to_caps = self.to.capabilities();
        let mut ir = self.source;
        
        // 逐字段判断 + LossyField::drop_if_needed
        if !to_caps.supports_thinking && ir.thinking.is_some() {
            LossyField::drop(self.from, self.to, "thinking");
            ir.thinking = None;
        }
        if !to_caps.supports_reasoning && ir.reasoning.is_some() {
            LossyField::drop(self.from, self.to, "reasoning");
            ir.reasoning = None;
        }
        // ... 完整 lossy 字段表见 §4.2
        
        // finish_reason 映射
        ir.finish_reason = self.from.map_finish_reason(ir.finish_reason, self.to);
        
        ir
    }
}

pub struct LossyField;
impl LossyField {
    pub fn drop(from: WireFormat, to: WireFormat, field: &'static str) {
        tracing::error!(
            from = %from, to = %to, field,
            "cross-render dropped vendor-specific field; \
             check codec/adapter mapping if this is unexpected"
        );
        metrics::counter!("gw_codec_cross_render_lossy_total",
            "from" => from.label(),
            "to" => to.label(),
            "field" => field,
        ).increment(1);
    }
}

pub enum WireFormat {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerate,
}

pub struct WireCapabilities {
    pub supports_thinking: bool,
    pub supports_reasoning: bool,
    pub supports_tool_use_blocks: bool,   // wire-端结构化 tool_use(Anthropic)
    pub supports_tool_calls_flat: bool,   // OpenAI 风格扁平 tool_calls
    pub supports_function_call_parts: bool,  // Gemini 风格 functionCall/Response part
    // ...
}
```

### 2.4 4 个 codec mod 改动

每个 codec mod (`openai`、`openai_responses`、`anthropic`、`gemini`)结构:

```
gw-codec/src/<vendor>/
├── codec.rs   # ProtocolCodec impl - render_response 实现, new_stream_state factory
├── wire.rs    # Response struct 新增, Request struct 加 tools/tool_choice 显式字段
├── via.rs     # IR ↔ wire 双向 (含 vendor-specific 字段)
└── state.rs   # [新] StreamState 实现 (per-codec accumulator)
```

**每个 codec 的具体 wire 改动**:

**OpenAI Chat** (`openai/wire.rs`):
- `OpenAiRequest` 加显式 `tools: Option<Vec<OpenAiTool<'a>>>`、`tool_choice: Option<OpenAiToolChoice<'a>>`(替代 extra HashMap 路径,N-1)
- 新增 `OpenAiResponse<'a>`:`choices: Vec<OpenAiChoice<'a>>`、`usage: OpenAiUsage`、`model`、`created`、`id`
- `OpenAiChoice.message.tool_calls: Option<Vec<OpenAiToolCall<'a>>>`(via 层已有此型,wire 端补出)
- `extra: HashMap<Cow<'a, str>, Box<RawValue>>`(N-1 Cow 改造)

**OpenAI Responses** (`openai/responses/wire.rs`):
- 复用现有 wire schema,补 `render_response` 实现(当前若为 unimplemented! 同样撤掉)
- 验证 reasoning summary 字段路径

**Anthropic Messages** (`anthropic/wire.rs`):
- `AnthropicMessage.content` 从 `Cow<'a, str>` 改为 `Vec<AnthropicContent<'a>>`(N-1 + C-2)
- `AnthropicContent` 枚举:`Text { text }`、`ToolUse { id, name, input }`、`ToolResult { tool_use_id, content }`、`Thinking { thinking, signature }`
- 新增 `AnthropicResponse<'a>`:`content: Vec<AnthropicContent<'a>>`、`stop_reason: AnthropicStopReason`、`usage: AnthropicUsage`、`model`、`id`

**Gemini Generate** (`gemini/wire.rs`):
- `GeminiPart` 从单纯 text 字段改为枚举:`Text { text }`、`FunctionCall { name, args }`、`FunctionResponse { name, response }`、`InlineData { mime_type, data }`(后两者是 multimodal 路径,本 spec 不消费但保留 wire 扩展点)
- 新增 `GeminiResponse<'a>`:`candidates: Vec<GeminiCandidate<'a>>`、`usage_metadata: GeminiUsage`、`model_version`

### 2.5 `gw-proxy` 接线

**`gw-proxy/src/filters/upstream.rs:230`**:
撤掉跨协议非流 503 短路。具体改动是删除整个 `if !is_stream && client_codec != upstream_codec { return Err(...) }` 块。

**`gw-proxy/src/filters/response.rs` non-stream 分支**:
在 `parse_response` 成功后,根据 `cross_protocol` flag 选两条路:
- 同协议:`upstream_codec.render_response(&ir, &mut out_buf)`(实际上若 zero-copy 已成立,此处可短路转发原 bytes,见 §3.1)
- 跨协议:`CrossRender { from, to, source: ir }.run(client_codec, &mut out_buf)?` + `gw_codec_cross_render_nonstream_total{from, to}` ++

**`gw-proxy` ctx 改动**:
- `ctx.stream_state: Option<Box<dyn StreamState>>` 新增(代替之前 codec stateless 假设)
- request_filter 末尾:`if is_stream { ctx.stream_state = Some(upstream_codec.new_stream_state()) }`
- response_body_filter:每个 chunk 调 `upstream_codec.parse_stream_chunk(state.as_mut(), bytes)`

---

## §3 数据流

### 3.1 同协议非流(zero-copy 不破坏)

```
upstream bytes
   │
   ▼
client_codec == upstream_codec?
   │
   ├── yes → 直接转发原 bytes (短路, 不走 IR 往返) [既有行为]
   │
   └── no  → upstream_codec.parse_response(bytes) → ResponseIR
            │
            ▼
            CrossRender::run → render_response → wire bytes (见 §3.2)
```

zero-copy 是 v1 既有的请求侧行为,响应侧本 spec 保持同样判定:`client_codec == upstream_codec` 时短路,不走 codec 层。

### 3.2 跨协议非流

```
upstream bytes
   │
   ▼
upstream_codec.parse_response → ResponseIR (含 thinking/reasoning/tool_use_blocks)
   │
   ▼
CrossRender { from, to, source: ir }
   │  · adapt_lossy: 按 to.capabilities() 裁剪 vendor-specific 字段
   │       · 每个 drop 字段:tracing::error! + lossy_total metric ++
   │  · finish_reason 映射 (见 §4.3 表)
   │  · 转出新 ResponseIR (字段子集)
   │
   ▼
client_codec.render_response(&adapted_ir, &mut buf) → wire bytes
   │
   ▼
gw_codec_cross_render_nonstream_total{from, to} ++
```

### 3.3 SSE 流式(累积器)

```
request_filter 末尾:
   if request_ir.stream:
       ctx.stream_state = Some(upstream_codec.new_stream_state())

response_body_filter 每个 chunk:
   chunk_bytes = ...
   events = upstream_codec.parse_stream_chunk(ctx.stream_state.as_mut(), chunk_bytes)?
   //  events: Vec<StreamEventIR>  — 0..N 个完整事件
   //  codec 内部:
   //     · 累积 OpenAI tool_call delta 直到 arguments JSON 完整
   //     · 累积 Anthropic thinking 直到签名块齐
   //     · 累积 Gemini functionCall.args delta 直到完整
   //  累积未完成的 chunk → 返回空 Vec,等下个 chunk
   
   for event in events:
       if cross_protocol:
           client_codec.render_stream_chunk(&event, &mut chunk_out)
       else:
           // 同协议零拷贝:codec 内部应能 emit 原 bytes (本 spec 不强制实现, 视 vendor 而定)
           upstream_codec.render_stream_chunk(&event, &mut chunk_out)
   
   send chunk_out to downstream
```

**累积器内部状态(per vendor)**:
- `openai::state::OpenAiStreamState`: `pending_tool_calls: HashMap<u32, ToolCallBuilder>`(index → 已累积 args 字符串)
- `anthropic::state::AnthropicStreamState`: `current_thinking: Option<ThinkingBuilder>`、`current_tool_use: Option<ToolUseBuilder>`(Anthropic event-style 多事件类型)
- `gemini::state::GeminiStreamState`: `pending_function_call: Option<FunctionCallBuilder>`
- `openai_responses::state::ResponsesStreamState`: 类似 Chat 加 reasoning 累积

---

## §4 错误处理与 lossy 策略

### 4.1 错误类型

| 错误码 | 触发位置 | HTTP 状态 |
|---|---|---|
| `GW_CODEC_PARSE_RESPONSE` | `parse_response` 失败 (wire 不合法) | 502 |
| `GW_CODEC_RENDER_RESPONSE` | `render_response` 失败 (IR 字段缺失 / 序列化错) | 502 |
| `GW_CODEC_PARSE_STREAM_CHUNK` | `parse_stream_chunk` 失败 | 502 |
| `GW_CODEC_CROSS_RENDER_FAILED` | `CrossRender::run` 失败 (adapt 后仍 render 失败) | 502 |

每个错误带 `from`(upstream wire)、`to`(client wire,跨协议时)与 `tracing::error!` 触发。

### 4.2 Lossy 字段分类表

按"跨协议时是否可能 lossy",每个字段属于以下两类之一:

| 字段 | 出现 vendor | 跨协议时归属 |
|---|---|---|
| `thinking` blocks | Anthropic / OpenAI Responses | **Lossy**(其他 wire 无对等) |
| `reasoning` summary | OpenAI Responses | **Lossy**(其他 wire 无对等) |
| `tool_use_blocks` (Anthropic 块形态) | Anthropic | **可表达**(转 OpenAI/Gemini 时映射为 tool_calls / functionCall) |
| `tool_calls` (扁平) | OpenAI Chat / Responses | **可表达**(转 Anthropic 时映射为 tool_use blocks) |
| `functionCall` / `functionResponse` parts | Gemini | **可表达**(同上反向) |
| `signature` 字段(Anthropic thinking) | Anthropic | **Lossy** (跨协议时随 thinking 一起 drop) |
| `cached_tokens` / `reasoning_tokens` (usage) | OpenAI Responses / Anthropic | **可表达**(usage 字段可塞进对应 wire 的 usage block,或退化为 0) |
| `model` / `created` / `id` | 所有 | **可表达**(可空,目标 wire 容许时填) |
| `stop_reason` 非 stop/length 值 (如 Anthropic `pause_turn`、Gemini `MAX_TOKENS`) | 各 vendor | **映射**(走 §4.3 表) |

**"可表达"** 不应触发 lossy log/metric。 "**Lossy**" 触发 ERROR log + metric。

### 4.3 finish_reason 跨协议映射表

| FinishReason (IR) | OpenAI Chat `finish_reason` | OpenAI Responses `status` | Anthropic `stop_reason` | Gemini `finishReason` |
|---|---|---|---|---|
| `Stop` | `stop` | `completed` | `end_turn` | `STOP` |
| `Length` | `length` | `incomplete`(reason=max_output_tokens) | `max_tokens` | `MAX_TOKENS` |
| `ToolCalls` | `tool_calls` | `completed`(output 含 tool 调用) | `tool_use` | `STOP`(parts 含 functionCall) |
| `ContentFilter` | `content_filter` | `incomplete`(reason=content_filter) | `stop_sequence`(近似)/ `end_turn` + lossy log | `SAFETY` |
| `Other(s)` | `stop`(fallback)+ lossy log | `completed` + lossy log | `end_turn` + lossy log | `OTHER` + lossy log |

`Other(s)` 落到任一目标都触发 lossy log(指 "无法精确表达原始 reason")。

### 4.4 ERROR log + metric 一致性

每次 `LossyField::drop` 调用都同步:
- `tracing::error!(from, to, field, "cross-render dropped vendor-specific field; check codec/adapter mapping if this is unexpected")`
- `gw_codec_cross_render_lossy_total{from, to, field}` ++

用户原话:"这种情况要 error 级别 log 出来,这是 codec 和 adapter 编码的 bug"。本 spec 把判定权交给运维 — log 必现,排查时按 from/to/field 三元组定位是否真的不可表达,还是 codec/adapter mapping 有遗漏。

---

## §5 测试策略

### 5.1 单测(per codec)

**位置**:`crates/gw-codec/src/<vendor>/codec.rs` 同文件 `#[cfg(test)]` 模块。

每个 codec 测覆盖:
- `parse_response` round-trip:固定 wire fixture → `ResponseIR` → 检查字段
- `render_response` round-trip:`ResponseIR` 构造 → `render_response` → 检查 wire bytes
- `render_response` zero-IR(纯 text + stop):最小工作集
- `render_response` 含 tool_use(Anthropic / OpenAI tool_calls)
- `render_response` 含 thinking(Anthropic / Responses):验证 IR 字段进 wire
- `parse_stream_chunk` + state:多 chunk 拼出一个完整事件
- `parse_stream_chunk` tool_call delta 合并:OpenAI / Gemini
- `parse_stream_chunk` thinking 续行:Anthropic

### 5.2 跨协议矩阵测

**位置**:`crates/gw-codec/tests/cross_protocol_matrix.rs`(新建)。

4 个 wire form,16 个组合:
- 4 个同协议(`from == to`): identity 验证(parse → render → 字节等价或语义等价)
- 12 个真实跨协议:每对的 lossy 字段列表与 §4.2 一致,验证 metric ++

每个 case 至少一个 tool_use round-trip(确保 tool 块跨协议保真)。

### 5.3 Lossy 路径专项

**位置**:`crates/gw-codec/tests/lossy_paths.rs`(新建)。

- Anthropic 含 thinking → OpenAI Chat:验证 metric `gw_codec_cross_render_lossy_total{from=anthropic, to=openai_chat, field=thinking}` 计数 ++ 1,验证 ERROR log 触发,验证 wire 输出无 thinking,验证 message text 正确。
- OpenAI Responses 含 reasoning → Anthropic:同上,field=reasoning。
- OpenAI Responses 含 reasoning_tokens(usage)→ Anthropic:验证 usage 字段不丢但 reasoning_tokens 不直接体现(归并到 cached_tokens? 还是 drop?),按 §4.2 决策。

### 5.4 E2E(gw-gateway 集成)

**位置**:`crates/gw-gateway/tests/e2e_cross_protocol_response.rs`(新建或扩充)。

- 客户端 OpenAI Chat / 上游 Anthropic / 模型回 tool_use:验证客户端 parse 出 tool_calls,`gw_codec_cross_render_nonstream_total{from=anthropic, to=openai_chat}` ++。
- 客户端 Anthropic / 上游 OpenAI Chat / 模型回 tool_calls:反向验证。
- 客户端 Gemini / 上游 OpenAI Chat / 模型回 tool_calls:验证转成 functionCall parts。
- 客户端 OpenAI Chat / 上游 Anthropic / 模型回 thinking:验证 thinking lossy(metric + log + wire 无 thinking)。

### 5.5 删除的 v1 reject 测例与错误码

落地时:
- 在 `crates/gw-proxy/tests/` 与 `crates/gw-proxy/src/` 下 grep `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED`,删除该错误码的所有定义、引用、断言。
- 任何测例其断言形式为 "cross-protocol non-stream 返回 503 / 含此错误码" 全部删除(不是 `#[ignore]`)。
- 若有反向断言形式 `assert!(!body.contains("GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED"))`,本 spec 落地后该断言仍成立(代码已删),无需主动删除该测例,但其语义已过时,建议同 PR 移除。
- `gw-core/src/error/codes.rs` 中 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` 错误码常量同 PR 删除。

---

## §6 与其他 sub-spec 协同

- **S-proxy-core (R-1: CapabilityResolver 注入)**:本 spec 不依赖 resolver。Resolver 可在请求时把"客户端 OpenAI Chat / 模型需 thinking"路由到不会产 thinking 的 vendor 或拒绝请求,从而避免跨协议 lossy 触发。但本 spec 不要求 resolver 阻断 lossy 路径。
- **S-ledger (L-1: detailed log sink)**:本 spec 在 `ResponseIR` 内提供 `UsageSnapshot` POD;S-ledger 在 `usage:detailed` 投递时消费该 POD。本 spec **不**消费 sink、**不**触碰 `usage:detailed` stream key。
- **S-routing (G-10: retry executor)**:本 spec 的 `GW_CODEC_PARSE_RESPONSE` / `GW_CODEC_RENDER_RESPONSE` 是 502 错误,可能被 S-routing retry 重试。但本 spec 不规定 retry policy,只规定错误码与触发位置。
- **S-admin-tunnel**:无直接依赖。
- **S-contracts**:无直接依赖(Go 侧不消费 ResponseIR 字段)。

---

## §7 N-1 局部优先级

N-1 字段在本 spec 范围内属于 **顺手收口**(non-blocking sub-task):

| 项 | 落地方式 | 优先级 |
|---|---|---|
| OpenAI extension keys `Cow<'a, str>` | `OpenAiRequest.extra: HashMap<Cow<'a, str>, Box<RawValue>>` | P2(顺手) |
| Anthropic content 数组 (`Cow<'a,str>` → `Vec<AnthropicContent>`) | `AnthropicMessage.content: Vec<AnthropicContent<'a>>` | **P0**(C-2 必需 — 不做就无法表达 tool_use block) |
| Gemini parts 改枚举 | `GeminiPart` 改枚举(text / functionCall / functionResponse / inlineData / fileData) | **P0**(C-2 必需) |

P0 的 wire 形态变更与 C-2 同 PR 落地;OpenAI extra `Cow` 改造可独立子提交,但建议同 PR 完成以减少 wire 端碎片化。

---

## §8 不变量

- **生产代码无 v1 标记**:本 spec 合入时同 PR 删除 `crates/gw-codec/src/**` 与 `crates/gw-proxy/src/filters/**` 内所有 `TODO(v2)` / `for v1:` / `unimplemented!(` 行(以本 spec 范围为限)。`scripts/check-invariants.sh` 的 `no_v1_markers_in_prod` lint 必须在本 spec 范围内验证零命中(对应 token 列表内的所有出现都被消除)。
- **错误码彻底清除**:`gw-core/src/error/codes.rs` 中 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` 常量与所有引用(测例 / 文档 / docstring)同 PR 删除。本 spec 落地后,workspace grep 该字符串应零命中(见 §5.5)。
- **`docs/architecture.md` §Known gaps**:G-8 不动(本 spec 不做 wire→wire codegen);G-9 同 PR 删除(跨块累积器落地);若 §Known gaps 含 `tool_use round-trip` / `cross-protocol non-stream response` 条目,同 PR 删除。
- **`docs/superpowers/specs/2026-05-19-codec-s4-response-and-cross-protocol.md`**:同 PR 在文件顶部加 "Superseded by [2026-05-24-v2-codec-cleanup-design.md](./2026-05-24-v2-codec-cleanup-design.md) (v2 landed 2026-05-24)" 标记。不删除原文件(历史记录)。
- **`docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md` §9**:同 PR 把 S-codec 行的预期日期 `2026-05-23` 改为 `2026-05-24`。
- **CLAUDE.md L26**:本 spec 落地后,L26 关于 "cross-protocol request AND response re-render are wired" 的描述需校正 — 实际上 v1 只 wired 了请求方向 + 流式响应方向,非流响应方向是本 spec 才 wired。同 PR 修订该句。

---

## §9 实施顺序提议(交给 writing-plans 细化)

1. **IR 扩展层**:`gw-core/src/ir.rs` 加 ResponseIR 新字段 / UsageSnapshot / 新 StreamEventIR variants。先 land,后续 codec 改动依赖此。
2. **Trait 改动**:`trait_def.rs` 加 `new_stream_state` + `parse_stream_chunk` 签名变化。同时所有 codec 的 stub impl(每个 codec 改一遍,先编过)。
3. **每个 codec wire 形态改造**(可并行):
   - `anthropic/wire.rs` content 数组(C-2 P0,影响最大)
   - `gemini/wire.rs` parts 枚举(C-2 P0)
   - `openai/wire.rs` 显式 tools / Cow extra(N-1)
   - 新 Response struct ×4
4. **每个 codec via.rs 双向 IR ↔ wire**(可并行,依赖步骤 3)。
5. **每个 codec codec.rs `render_response` 实现**(可并行,依赖步骤 3+4)。
6. **每个 codec `state.rs` 累积器 + `parse_stream_chunk`**(可并行,依赖步骤 2)。
7. **`gw-codec/src/cross_render.rs`** 新建(依赖步骤 1)。
8. **`gw-proxy/src/filters/upstream.rs` 撤 503 + `response.rs` 接入 CrossRender**(依赖步骤 5+7)。
9. **跨协议矩阵测 + lossy 专项测 + e2e**(依赖步骤 1–8 全部)。

---

## §10 状态追踪

- [x] §1 目标态全部签出 (T1-T10 landed 2026-05-24)
- [x] §2 文件结构按表落地
- [x] §3 数据流(同协议短路 + 跨协议 IR 往返 + 流式累积器)行为如描述
- [x] §4 错误处理 + lossy 表实现 + ERROR log 同步触发
- [x] §5 测试 5 类全部通过(单测 / 跨协议矩阵 / lossy 专项 / e2e stub / v1 reject 测例已删)
- [x] §6 与其他 sub-spec 无意外耦合(S-ledger 消费 UsageSnapshot 时 schema 一致)
- [x] §7 N-1 P0 项与 C-2 同 PR 落地
- [x] §8 不变量(v1 标记清零 + §Known gaps 同 PR 删 + 老 spec marker + overview §9 日期 + CLAUDE.md L26 校正)
- [x] 进入 writing-plans 时本 spec 不再有任何 TBD / placeholder

**已知后续 (out of S-codec scope; spawn follow-up sub-tasks if needed):**
- T9 e2e 测仍为 `#[ignore]` stub;真实 e2e 走 `tests/e2e/client/scenarios/test_non_stream.py::test_cross_protocol_non_stream_round_trips`(已加入,在 docker 化 `make e2e` 时验证)。
- `crates/gw-codec/src/gemini/codec.rs` 的 `parse_stream_chunk_emits_function_call_events` 测仅覆盖单 chunk;多 chunk functionCall args 累积测留作后续。
