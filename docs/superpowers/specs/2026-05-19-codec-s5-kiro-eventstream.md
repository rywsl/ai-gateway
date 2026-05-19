# Codec S5 — Kiro EventStream Codec(A-T7)(v2026-05-19)

- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md)(**硬前置**:wire_convert_matrix!)
  - [2026-05-19-codec-s3-sse-multi-event.md](./2026-05-19-codec-s3-sse-multi-event.md)(SSE 多事件 trait + framer 上移)
  - [2026-05-19-adapter-a-t6-kiro-vendor.md](./2026-05-19-adapter-a-t6-kiro-vendor.md)(A-T6 vendor adapter,本 spec 配套)
  - Aether 来源:`src/services/provider/adapters/kiro/parser/`、`eventstream_rewriter.py`
- **范围**：在 codec 矩阵新增 Kiro wire 类型:
  - 请求侧 wire:`KiroRequest`(Anthropic Messages 兼容 + Kiro envelope 字段);
  - 响应侧 wire:`KiroResponse`(非流场景,与 Anthropic 同形);
  - 流式 wire:`KiroStreamFrame`(AWS EventStream 二进制帧);
  - EventStream binary framer + decoder;
  - thinking tag 解析与 buffer(文本内嵌的 `<thinking>` 标签 → ContentPartSchema::Thinking);
  - 加入 `wire_convert_matrix!`,自动获得 N² 与其他 wire 的互转。
- **不在范围**：
  - Kiro adapter(→ A-T6);
  - AWS SigV4 鉴权(→ A-T6 + gw-proxy);
  - 其他 vendor codec。

> A-T7 是 6 个 vendor 里**最大的工作单元**——AWS EventStream 协议 + Kiro 特有 envelope + thinking tag parser 三件套。它把"adapter v.s. codec"的边界推到极限:vendor 引入的"真正新协议"必须升级 codec 而不是塞 adapter。

---

## 1. 背景

AWS EventStream 是 AWS 自定义的二进制流协议(`application/vnd.amazon.eventstream`),帧格式:

```
[ total_byte_length (4B big-endian) ]
[ headers_byte_length (4B BE) ]
[ prelude_crc (4B BE) ]                         // CRC32 of preceding 8 bytes
[ headers (variable, name + type + value) ]
[ payload (variable) ]
[ message_crc (4B BE) ]                          // CRC32 of entire message
```

headers 类型集合(7 个类型,name 用 1B 长度前缀字符串)。

Kiro 用 EventStream 包装两类消息:
- `event` 类型 - 业务流(对应客户端 SSE 的 message_start / content_block_delta 等);
- `exception` 类型 - 错误流(对应客户端的 error 事件)。

payload 是 JSON 字符串(UTF-8),Aether `parser/decoder.py` 解。

此外 Kiro 响应里 `<thinking>...</thinking>` 标签是**文本中嵌入**的,而非结构化字段——需要 codec 解 tag → 拆出 thinking block + text block,而不是把 raw text 透传出去。

## 2. 设计决策

### ADR-AT7-1 wire 类型

新增三个 wire struct 进入 codec 矩阵:

```rust
// crates/gw-codec/src/kiro/wire.rs

/// Kiro 请求 wire——Anthropic Messages 兼容 + 顶层 envelope 字段
#[derive(Debug, Deserialize, Serialize)]
pub struct KiroRequest<'a> {
    /// Anthropic 标准字段全部支持(从注解推导跨 wire 字段对齐)
    #[ir(map = "model")]
    pub model: Cow<'a, str>,

    #[ir(map = "messages", via = "kiro_messages")]
    pub messages: Vec<KiroMessage<'a>>,

    #[ir(map = "max_tokens")]
    pub max_tokens: u32,

    #[ir(map = "temperature")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    // ... 其他 Anthropic 兼容字段

    /// Kiro envelope 顶层字段——通过 #[ir(skip, only(out))] 仅在渲染时填,
    /// 解析时若客户端真发了 Kiro 请求(罕见)直接接收。
    #[ir(skip, only(out))]
    pub conversation_id: Option<Cow<'a, str>>,

    #[ir(skip, only(out))]
    pub session_id: Option<Cow<'a, str>>,

    // ...其他 Kiro 特有字段
}

/// Kiro 响应非流 wire——形态 = AnthropicResponse + Kiro 元数据
#[derive(Debug, Deserialize, Serialize)]
pub struct KiroResponse<'a> {
    // 通常 Kiro 不返回非流响应(只有 stream)——但若有,沿用 Anthropic 形态
    #[ir(map = "id")]
    pub id: Cow<'a, str>,
    #[ir(map = "model")]
    pub model: Cow<'a, str>,
    #[ir(map = "messages", via = "kiro_response_messages")]
    pub content: Vec<KiroResponseBlock<'a>>,
    // ...
}

/// Kiro 流式帧——这是 AWS EventStream 解码后的一帧
#[derive(Debug, Deserialize, Serialize)]
#[ir(stream = true, emit_multiple)]
pub struct KiroStreamFrame<'a> {
    /// EventStream 头部 `:event-type`("event" / "exception" 等)
    pub event_type: Cow<'a, str>,
    /// EventStream 头部 `:message-type`("event" / "error")
    pub message_type: Cow<'a, str>,
    /// payload 解出来的 JSON 内容(typed by event_type)
    pub payload: KiroEventPayload<'a>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum KiroEventPayload<'a> {
    /// 业务消息:对应 Anthropic SSE message_start / content_block_delta / ...
    Message(KiroMessageEvent<'a>),
    /// 错误消息
    Exception(KiroExceptionEvent<'a>),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct KiroMessageEvent<'a> {
    /// 文本增量;包含 <thinking>...</thinking> 内嵌标签
    pub assistant_response_message: Option<KiroAssistantResponseMessage<'a>>,
    pub usage: Option<KiroUsage>,
    // ...
}
```

### ADR-AT7-2 EventStream framer

```rust
// crates/gw-codec/src/kiro/framer.rs

pub struct EventStreamFramer {
    /// 累积 buffer:incoming bytes 进来攒到完整一帧(含 prelude + headers + payload + crc)
    buf: Vec<u8>,
}

impl EventStreamFramer {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<EventStreamMessage> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 12 { break; }  // 至少需要 prelude (12 bytes)
            let total = u32::from_be_bytes(self.buf[0..4].try_into().unwrap()) as usize;
            if self.buf.len() < total { break; }  // 帧未完整

            let frame_bytes: Vec<u8> = self.buf.drain(..total).collect();
            match parse_message(&frame_bytes) {
                Ok(msg) => out.push(msg),
                Err(e) => {
                    tracing::warn!(error=?e, "kiro eventstream frame parse failed");
                    // 跳过损坏帧,继续 framer 状态
                }
            }
        }
        out
    }
}

pub struct EventStreamMessage {
    pub headers: HashMap<String, EventStreamHeaderValue>,
    pub payload: Vec<u8>,
}

pub enum EventStreamHeaderValue {
    Bool(bool),
    Byte(u8),
    Short(i16),
    Integer(i32),
    Long(i64),
    ByteArray(Vec<u8>),
    String(String),
    Timestamp(i64),
    Uuid(uuid::Uuid),
}

fn parse_message(frame: &[u8]) -> Result<EventStreamMessage, FramerError> {
    // 1. 解 prelude (total, header_len, prelude_crc) + verify CRC32
    // 2. 解 headers(变长)
    // 3. payload = headers 结束位置到 (total - 4) 之间
    // 4. message_crc = frame[total-4..total],verify
    // 5. 构造 EventStreamMessage
    ...
}
```

CRC32 用 `crc32fast` crate。

### ADR-AT7-3 trait 接入(codec S3 模型)

S3 已经规划了 trait:

```rust
fn parse_stream_chunk_to<B: WireStreamTarget>(
    &self,
    chunk: &[u8],
    accum: &mut B::Accum,
    out: &mut B::ChunkBuilder,
) -> Result<StreamProgress, ErrorIR<'static>>;
```

Kiro 的实现:

```rust
// crates/gw-codec/src/kiro/codec.rs

pub struct KiroCodec {
    framer: Mutex<EventStreamFramer>,  // adapter/proxy 在 ctx 上持框架,这里参考概念
}

impl ProtocolStreamCodec for KiroCodec {
    fn parse_stream_chunk_to<B: WireStreamTarget>(
        &self,
        chunk: &[u8],
        accum: &mut B::Accum,
        out: &mut B::ChunkBuilder,
    ) -> Result<StreamProgress, ErrorIR<'static>> {
        // 1. framer.feed(chunk) → 0..N 个 EventStream message
        // 2. 每个 message → parse 出 KiroStreamFrame
        // 3. 用 codegen 出来的 kiro_to_<B>_chunk 函数转目标 wire
        //    (例如 kiro_to_anthropic_chunk → AnthropicStreamFrame)
        // 4. 写入 out (ChunkBuilder)
        // 5. 收到 response.completed 或 exception → return StreamProgress::Done/Errored
        ...
    }
}
```

`framer` 状态归属:per-request 在 gw-proxy `StreamRelay` 持有(参 codec S3 §ADR-S3-v2-4)。Kiro 的 framer 也走这套——framer 实际位置在 gw-proxy/gw-task-runtime 的 `StreamRelay::framer`,**不在 codec 内**。但 EventStream framer 的实现(parse_message 函数等)放 codec mod 共享。

### ADR-AT7-4 thinking tag 解析(关键)

Kiro 响应文本里嵌入 `<thinking>...</thinking>` 标签,需要在 codec 转换时拆出来:

```rust
// crates/gw-codec/src/kiro/thinking_parser.rs

pub struct ThinkingTagParser {
    /// 已收到但未拼接完成的 partial tag(跨 chunk 时)
    pending: String,
    /// 当前是否在 thinking 标签内
    in_thinking: bool,
    /// thinking 缓冲(最多 1MB 防内存爆)
    thinking_buffer: String,
}

const MAX_THINKING_BUFFER: usize = 1024 * 1024;

impl ThinkingTagParser {
    pub fn feed<'a>(&mut self, chunk: &'a str) -> Vec<ParsedSegment<'a>> {
        // 状态机:扫描 chunk,识别 <thinking> 与 </thinking>;
        // 拆为 Text("..."), ThinkingStart, ThinkingDelta("..."), ThinkingEnd, Text("...");
        // 跨 chunk 时部分匹配("<th" + "inking>")由 pending 缓存。
        ...
    }
}

pub enum ParsedSegment<'a> {
    Text(&'a str),
    ThinkingStart,
    ThinkingDelta(Cow<'a, str>),
    ThinkingEnd,
}
```

这套解析在 `kiro_to_<B>_chunk` 直转里调:把 Text 写到 B 的 content 字段,Thinking* 写到 B 的 thinking 字段(对齐 schema)。

### ADR-AT7-5 加入矩阵宏

```rust
// crates/gw-codec/src/matrix.rs(扩展)

wire_convert_matrix! {
    schema: RequestSchema,
    wires: [
        openai::wire::OpenAiRequest,
        anthropic::wire::AnthropicRequest,
        gemini::wire::GeminiRequest,
        kiro::wire::KiroRequest,
    ],
}

wire_convert_matrix! {
    schema: ResponseSchema,
    wires: [
        openai::wire::OpenAiResponse,
        anthropic::wire::AnthropicResponse,
        gemini::wire::GeminiResponse,
        kiro::wire::KiroResponse,
    ],
}

wire_convert_matrix! {
    schema: StreamChunkSchema,
    wires: [
        openai::wire::OpenAiStreamChunk,
        anthropic::wire::AnthropicStreamFrame,
        gemini::wire::GeminiStreamChunk,
        kiro::wire::KiroStreamFrame,
    ],
    stream: true,
}
```

N=4 后:请求矩阵 16 个直转(含 4 个同协议恒等);响应同;流式同。

注意:**客户端通常不主动发 Kiro 协议请求**——客户端是 Anthropic / OpenAI,Kiro 是上游。所以 KiroRequest 主要走"出方向"(`From<AnthropicRequest> for KiroRequest`);"入方向"(`From<KiroRequest> for OpenAiRequest`)实现但极少触发。

### ADR-AT7-6 via 模块

```rust
// crates/gw-codec/src/kiro/via.rs

pub mod kiro_messages {
    // 把 schema messages → KiroMessage(同 anthropic 同款,可以复用 anthropic_messages)
}

pub mod kiro_response_messages {
    // 把 KiroResponse content blocks → schema messages
    // **关键**:这里运行 thinking_parser 把 `<thinking>` 拆出来
}

pub mod cross {
    // Kiro ↔ Anthropic 直转(最常用——Kiro 兼容 Anthropic,几乎 1:1)
    pub fn anthropic_to_kiro<'a>(src: AnthropicRequest<'a>) -> KiroRequest<'a> { ... }
    pub fn kiro_to_anthropic<'a>(src: KiroResponse<'a>) -> AnthropicResponse<'a> { ... }
    // Kiro ↔ OpenAI 直转(走 schema)
    // Kiro ↔ Gemini 直转(走 schema)
}
```

### ADR-AT7-7 错误事件

Kiro 的 exception event:

```json
{
  "__type": "ValidationException",
  "message": "Invalid input"
}
```

codec 把它解析为 `StreamEventSchema::Error(ErrorSchema)`,再由 N² 直转写到目标 wire 的错误帧(Anthropic 的 `event: error`,OpenAI 的 `data: {"error": ...}`)。

### ADR-AT7-8 性能预算

| 操作 | 预算 |
|---|---|
| EventStream framer feed(1 KB chunk) | < 5 µs |
| parse_message(1 KB frame) | < 10 µs(含 CRC32 验证 × 2) |
| thinking_parser feed | < 2 µs / KB |
| kiro_to_anthropic 直转(单 chunk) | < 5 µs |

CRC32 必须用硬件加速(`crc32fast` crate 自动用 SSE4.2 if available)。

### ADR-AT7-9 测试

- EventStream framer:已知二进制帧 → 解出正确 headers/payload;
- framer:CRC 不匹配 → 跳过帧 + warn;
- framer:跨 chunk 帧(前半 + 后半)→ feed 两次组装;
- thinking_parser:文本中带 `<thinking>...</thinking>` → 拆出三段(text/thinking/text);
- thinking_parser:跨 chunk 标签(`<th` + `inking>`)→ 正确拼接;
- thinking_parser:malformed tag(`<thinking>` 无关闭)→ 1MB buffer 上限保护 + warn;
- kiro_to_anthropic 单 chunk → AnthropicStreamFrame 形态正确;
- 错误事件 → ErrorSchema 转给客户端 error 帧;
- 与 anthropic 客户端的端到端 round-trip(需 mock Kiro endpoint)。

## 3. 实施大纲

依赖:codec S1/S2/S3 land + `crc32fast`、`uuid` crate 已有或新增。

1. `crates/gw-codec/src/kiro/` 模块;
2. wire.rs(KiroRequest / KiroResponse / KiroStreamFrame + 注解);
3. framer.rs(EventStream binary parser);
4. thinking_parser.rs(tag 状态机);
5. via.rs(schema 与 cross::*);
6. codec.rs(`KiroCodec` 实现 `ProtocolStreamCodec`);
7. matrix.rs 扩列;
8. workspace `Cargo.toml` 加 `crc32fast`;
9. 单测覆盖所有场景。

## 4. 未决问题

1. **是否要给 Kiro 加独立 `ProtocolKind`**:目前 codec 三协议 enum(OpenAi/Anthropic/Gemini)。加 Kiro 是否升 ProtocolKind?Kiro 客户端不会主动用 Kiro 协议(它是上游),所以 ProtocolKind 可能不必加 variant。倾向 v1 `ProtocolKind::Anthropic` 同款标(因为 Kiro 兼容 Anthropic),Kiro 只在 wire 矩阵作为"上游侧" wire 存在。
2. **thinking_buffer 1MB 上限**:Aether 沿用。是否要可配置?倾向 const + env override(`GW_CODEC__KIRO__MAX_THINKING_BUFFER`)。
3. **EventStream 头部类型完整解析**:Aether 只用 `:event-type` 与 `:message-type` 两个 String 头。v1 codec 只 parse 这两个头,其他 header 类型实现但不用。完整支持留 follow-up。
4. **CRC 验证失败时**:跳过帧 + warn 是 v1 行为。可能需要更激进的"流终止"(防止 corrupt stream 产生误 thinking 输出),但 trade-off 是单帧错误连累整流。倾向 v1 跳过 + warn,monitor `gw_codec_kiro_crc_fail_total`。
5. **KiroRequest 入方向的实用性**:客户端真的会发 Kiro 协议吗?可能性极低。若不会,KiroRequest 可以 `direction = "to"`(只发不收),节省一半 codegen。倾向标 `direction = "to"`,N² 减少。
6. **同矩阵 N=4 影响整体编译时间**:codec S1 在 N=3 时已经评估编译时间;N=4 是 16 个直转 + 16 个 trait impl。预算应当还可接受,但需要实测。若编译时间膨胀 30%+,需要拆分编译单元(每对 (A→B) 一个 mod)。
7. **Kiro 与 task worker 的关系**:Kiro 没有 task(异步任务)语义,纯流式。task worker 接 Kiro 时仍是 stream → 累积聚合 → 终态保存。worker spec 与本 spec 无特殊耦合。

---

> A-T7 是 6 个 vendor 里**工作量最大**的——AWS EventStream + thinking parser + 矩阵 N=4 扩展。但完成后,Kiro 成为完整支持的 vendor;后续添加 Bedrock 等 AWS 上游可复用 EventStream framer。
