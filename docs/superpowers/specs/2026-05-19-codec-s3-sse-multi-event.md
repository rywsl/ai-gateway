# Codec S3 — 流式 N² Wire 直转 + 累积状态（v2026-05-19，v2 重写）

- **日期**：2026-05-19
- **状态**：Draft（v2 重写）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md)（**硬前置**：编译期 schema + 矩阵宏）
  - [2026-05-19-codec-s2-wire-multimodal.md](./2026-05-19-codec-s2-wire-multimodal.md)（请求侧 wire 形状参考）
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（C5）
- **范围**：流式 SSE chunk 的跨 wire N² 直转；codegen 为每对 wire 生成"带跨 chunk 累积状态"的流式直转函数；trait 升级为目标 wire 泛型化的 `parse_stream_chunk_to`；StreamEventSchema 仅作编译期元模型。
- **不在范围**：响应侧非流（→ S4）；framer 分帧（仍由 gw-proxy 实现，本 spec 不动）。

> **v2 重写说明**：v1 提议 trait 签名改 `SmallVec<[StreamEventIR; 4]>`，仍走 IR 中转。用户回："流式也走 N² 直转"。v2 形态：流式 wire 解析直接转为目标 wire 的 stream chunk，跨 chunk 累积状态由 codegen 生成的状态机承载，runtime 永不实例化 StreamEventSchema。

---

## 1. 背景

v1 现状：`parse_stream_chunk(chunk) -> Result<Option<StreamEventIR>>`，一进一出，无法表达 Gemini 结尾 chunk 同时含 text + finishReason + usage 三件事。

v2 形态约束：流式跨协议路径必须是 wire_a chunk → wire_b chunk，永不实例化 StreamEventSchema。但流式有跨 chunk 累积状态：
- thinking signature 在多个 chunk 上分块传输，最后拼接成完整 signature；
- tool_call_delta 在多个 chunk 上累加 arguments；
- Anthropic event-stream 的 `message_start` → `content_block_start` → `content_block_delta` → `message_delta` 多帧组合表达一次完整响应；
- OpenAI 结尾 chunk 可能合并 finish_reason + usage。

跨 wire 直转必须把这些状态机的语义穿越实现。这是流式 N² 直转的本质挑战。

## 2. 设计决策

### ADR-S3-v2-1 StreamEventSchema 与 StreamAccumSchema

S1 的 schema 模型扩展支持流式：

```rust
// gw-core::schema::stream

#[ir_schema(target = "stream_event", stream = true)]
#[non_exhaustive]
pub enum StreamEventSchema {
    Start { request_id: String },
    Delta(StreamDeltaSchema),
    Usage(UsageSchema),
    End { finish_reason: FinishReasonSchema },
    Error(ErrorSchema),
}

#[ir_schema(target = "stream_delta")]
#[non_exhaustive]
pub struct StreamDeltaSchema {
    pub text: Option<String>,
    pub thinking_text: Option<String>,
    pub thinking_signature_delta: Option<String>,  // 增量拼接
    pub tool_call: Option<ToolCallDeltaSchema>,
}

#[ir_schema(target = "stream_accum")]
#[non_exhaustive]
pub struct StreamAccumSchema {
    /// 当前正在累积的 thinking signature
    pub current_thinking_signature: String,
    /// 当前正在累积的 tool_calls，按 id 索引
    pub current_tool_calls: HashMap<String, ToolCallAccumSchema>,
    /// 是否已经发过 Start 事件
    pub started: bool,
    /// 已发送 chunk 计数（debug 用）
    pub chunks_emitted: u32,
}
```

`stream_accum` 是新概念——**跨 chunk 状态承载结构**。schema 标 `stream_accum = true` 告诉 codegen 这是状态机数据，需要在 wire→wire 直转时显式建模。

### ADR-S3-v2-2 trait 签名升级

```rust
// crates/gw-codec/src/trait_def.rs

pub trait ProtocolStreamCodec: Send + Sync + 'static {
    /// 跨 wire 流式直转：把本 codec 收到的 raw chunk 直接转换为目标 wire 形态。
    /// `accum` 是跨 chunk 状态，必须由调用方持有并在每次调用间传入。
    fn parse_stream_chunk_to<B: WireStreamTarget>(
        &self,
        chunk: &[u8],
        accum: &mut B::Accum,
        out: &mut B::ChunkBuilder,
    ) -> Result<StreamProgress, ErrorIR<'static>>;
}

pub enum StreamProgress {
    /// 已写出 0..N 个 chunk 到 out，stream 仍继续
    Continue,
    /// 已写出 stream 终止 chunk（[DONE] / final event），accum 应被丢弃
    Done,
    /// 上游流被错误终止
    Errored,
}

pub trait WireStreamTarget {
    type Accum: Default;
    type ChunkBuilder;

    fn finalize_chunk(builder: Self::ChunkBuilder, buf: &mut BytesMut)
        -> Result<(), ErrorIR<'static>>;
}
```

每个目标 wire 实现 `WireStreamTarget`：

```rust
impl WireStreamTarget for OpenAiStreamChunk<'_> {
    type Accum = OpenAiStreamAccum;
    type ChunkBuilder = OpenAiStreamChunkBuilder<'static>;

    fn finalize_chunk(builder: OpenAiStreamChunkBuilder, buf: &mut BytesMut) -> ...
}
```

`ChunkBuilder` 是一个临时写入器（不是完整 wire struct）；它支持 push_text / push_tool_delta / push_usage / push_finish 等方法，每次写入对应一行 `data: {...}\n\n` 到 buf。这样目标侧的多事件输出仍然是一次性写完，不实例化中间结构。

### ADR-S3-v2-3 codegen 为 N² 对生成流式直转

`wire_convert_matrix!` 在 `stream: true` 模式下，为每对 (A, B) wire 生成：

```rust
// codegen 输出示例：OpenAI → Anthropic 流式直转
impl ProtocolStreamCodec for OpenAi {
    fn parse_stream_chunk_to<B: WireStreamTarget>(
        &self,
        chunk: &[u8],
        accum: &mut B::Accum,
        out: &mut B::ChunkBuilder,
    ) -> Result<StreamProgress, ErrorIR<'static>> {
        // codegen 根据 B 的具体类型 dispatch 到不同代码：
        match TypeId::of::<B>() {
            id if id == TypeId::of::<OpenAiStreamChunk<'_>>() => {
                // 同协议：直接 forward chunk，更新 accum
                openai_to_openai_chunk(chunk, accum, out)
            }
            id if id == TypeId::of::<AnthropicStreamFrame<'_>>() => {
                // OpenAI → Anthropic：parse OpenAI chunk → 写 Anthropic frame
                openai_to_anthropic_chunk(chunk, accum, out)
            }
            id if id == TypeId::of::<GeminiStreamChunk<'_>>() => {
                openai_to_gemini_chunk(chunk, accum, out)
            }
            _ => unreachable!("WireStreamTarget not in matrix"),
        }
    }
}
```

每对 `<src>_to_<dst>_chunk` 函数由 codegen 生成，内部按 schema 标注的 stream_delta 字段对齐执行字段拷贝 + 累积状态更新。

具体生成逻辑：

1. codegen 读 src wire 的 stream chunk 结构 + `#[ir(...)]` 注解；
2. 读 dst wire 的 stream chunk 结构 + 注解；
3. 对每个 stream_delta 字段（text / thinking_text / tool_call / finish_reason / usage）：
   - src 字段 → accum 状态 → dst 字段；
   - 例如 thinking_signature_delta：src 写入 accum.current_thinking_signature；某帧（end-of-thinking 帧）触发 accum 拼接完整 signature → 写入 dst 的 finalize_thinking chunk。
4. dst 不支持的字段（如 OpenAI → Anthropic 的 finish_reason 名字变化）走 schema 标注的 finish_reason 映射函数。

### ADR-S3-v2-4 framer 上移到 gw-proxy

S3 v2 与 v1 一致：单 chunk 解析的边界由 framer 决定，framer 在 gw-proxy 层：

- OpenAI: `\n\n` 切；
- Gemini: `\r\n\r\n` 切；
- Anthropic event-stream: `event: <name>\n data: {...}\n\n` 切，先解 event 头再调 codec。

framer 状态机也持有 accum 引用：

```rust
// 伪代码 in gw-proxy
struct StreamRelay<S: ProtocolStreamCodec, T: WireStreamTarget> {
    src_codec: S,
    framer: Framer,
    accum: T::Accum,
    builder: T::ChunkBuilder,
}

impl<S, T> StreamRelay<S, T> {
    fn on_upstream_chunk(&mut self, bytes: &[u8], buf: &mut BytesMut) -> ... {
        for frame in self.framer.feed(bytes) {
            self.src_codec.parse_stream_chunk_to::<T>(
                &frame,
                &mut self.accum,
                &mut self.builder,
            )?;
            T::finalize_chunk(std::mem::take(&mut self.builder), buf)?;
        }
    }
}
```

### ADR-S3-v2-5 错误处理

- 单 frame 解析失败 → `Ok(StreamProgress::Continue)` + `tracing::warn!`（不短路流）；
- 结构性致命错误（chunk 完全不是 SSE）→ `Err(ErrorIR)`，由 gw-proxy 决定是否短路；
- accum 损坏（thinking signature 没收尾就收到 message_delta）→ 丢弃当前 thinking 块 + warn，继续后续。

### ADR-S3-v2-6 性能预算

| 路径 | 预算（per chunk） |
|---|---|
| Same-protocol passthrough（无 mutation） | < 200 ns（chunk 借引用直接 forward） |
| Cross-protocol（无 thinking signature） | < 2 µs |
| Cross-protocol（有 thinking signature 累积） | < 3 µs（多一次 String push） |

新增 bench：`gw-codec/benches/stream_n2_convert.rs`，覆盖 9 对 wire 的流式直转。

### ADR-S3-v2-7 schema 标注

参 S1 §ADR-S1-v2-3，stream chunk 形态在 schema 上额外标：

```rust
#[ir_schema(target = "stream_event", stream = true)]
pub enum StreamEventSchema { ... }

#[ir_schema(target = "stream_accum", stream_state = true)]
pub struct StreamAccumSchema { ... }
```

`stream = true` 让矩阵宏知道这是流式 schema，输出"按 chunk 增量"的转换函数；`stream_state = true` 让矩阵宏识别"跨 chunk 状态承载结构"，为每个 wire 派生 `*Accum` 类型。

## 3. 实施大纲

1. S1 v2 与 S2 v2 land；
2. 增 schema stream + accum 模块（§ADR-S3-v2-1）；
3. 改 trait 签名 → `parse_stream_chunk_to`（§ADR-S3-v2-2）；
4. 三家 stream wire 结构（OpenAiStreamChunk / AnthropicStreamFrame / GeminiStreamChunk）补全 + 标注；
5. `wire_convert_matrix!` 加 stream block 调用；
6. framer 上移到 gw-proxy（与 codec 解耦），见 `gw-proxy` 改造；
7. 三家 codec 提供 ChunkBuilder 实现；
8. 测试矩阵：
   - 9 对流式直转：每对至少 (a) 纯 text 流、(b) 含 thinking + signature 流、(c) 含 tool_call_delta 流、(d) 结尾合并 finish_reason + usage；
   - 累积状态损坏后的恢复（不应 panic）；
   - 性能 bench 守预算。

## 4. 未决问题

1. **`TypeId` dispatch 是否真的零开销**：codegen 在 `parse_stream_chunk_to` 内部用 TypeId match。理论上 monomorphization 后单态化每个 dispatch 都是直接调用，但需要 cargo bench 验证。倾向用 `#[inline(always)]` 让编译器消除 match。
2. **accum 与 builder 的 lifetime 约束**：accum 持有 owned String / Vec，builder 是借引用——builder 的 lifetime 是 chunk 的生命期。codegen 自动推断可能需要复杂 phantom lifetime。本 spec 推后到实施时定。
3. **OpenAI `stream_options.include_usage` 的开关**：客户端可能要求"非 usage 流"——这是 OpenAI 协议层面的开关，schema 应不应该承载？倾向 schema 上加 `stream_options: Option<StreamOptionsSchema>`，wire 上各自映射。
4. **Anthropic event-stream 的 `ping` 事件**：Anthropic 定期发心跳事件。framer 应该过滤还是 codec？倾向 framer——心跳是传输层语义，不进 codec。
5. **跨 chunk 状态的边界**：何时清空 accum？倾向：收到 StreamProgress::Done 时调用方清空；中途切上游（retry）时调用方手动清空（gw-proxy 责任）。
6. **schema `stream_accum` 与 codegen 推导的关系**：accum 字段（current_thinking_signature 等）是否需要也派生 trait？倾向不——accum 是 codegen 内部用，业务不直接访问。
7. **流式 mutation 是否支持**：业务 filter 能否在流式途中改 chunk（如脱敏 thinking）？倾向 v1 不支持流式 mutation；S3 仅做透传/转换。流式脱敏推到后续 spec。

---

> 本 spec 让流式跨协议也达成 N² wire→wire 直转，不再依赖 IR/schema 实例化。S4 接管响应侧非流。
