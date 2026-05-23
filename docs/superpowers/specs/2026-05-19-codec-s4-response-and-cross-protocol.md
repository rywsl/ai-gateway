# Codec S4 — 响应侧 N² Wire 直转 + UsageSnapshot（v2026-05-19，v2 重写）

> **Superseded by [2026-05-24-v2-codec-cleanup-design.md](./2026-05-24-v2-codec-cleanup-design.md) (S-codec sub-spec, landed 2026-05-24). All ADR-S4-v2-* items in this document are now implemented and tracked under the new spec.**

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft（v2 重写）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md)（**硬前置**：编译期 schema + 矩阵宏）
  - [2026-05-19-codec-s2-wire-multimodal.md](./2026-05-19-codec-s2-wire-multimodal.md)（**硬前置**：请求侧 wire 形状，含可复用 enum）
  - [2026-05-19-codec-s3-sse-multi-event.md](./2026-05-19-codec-s3-sse-multi-event.md)（流式互补）
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（C4 + 响应侧 C1/C2/C3）
- **范围**：
  - 三家响应侧 wire 结构定义（与 S2 请求侧对称）+ `#[ir(...)]` 注解；
  - N² 响应直转（codegen 通过 `wire_convert_matrix!` 生成）；
  - **撤回** `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` 400 短路决策；
  - `extract_usage_any(wire) -> UsageSnapshot` POD 提取函数供 billing；
- **不在范围**：OpenAI Responses 形状（→ C8）、计费具体公式（→ 桶 6 / gw-billing）。

> **v2 重写说明**：v1 仍以 `parse_response → ResponseIR → render_response` 双向 + IR 中转为模型。v2 全量 N² 直转 + UsageSnapshot POD 替代 IR；非流 400 短路 仍撤回。

---

## 1. 背景

v1 已规划响应侧 wire 形态 + finish_reason 跨协议映射 + UsageIR 多维扩展。v2 的形态变化：

- 响应侧 wire 不再标 `IrConvert`，改为标 `#[ir(...)]` 注解供矩阵宏读；
- 响应直转生成 N² 个 `impl From<A> for B` for response wire；
- UsageIR runtime 实例化（v1 选项）被撤回，改 `UsageSnapshot` POD（用户拍板的 "Usage snapshot 函数 + 轻量 POD"）；
- 撤回 nonstream 400 短路（v1 已规划）。

## 2. 设计决策

### ADR-S4-v2-1 撤回 nonstream 400 短路

`gw-proxy::filters::request.rs:233-249` 区域整段删除（保留行号供 review 检索）。

撤回的前提（S4 兑现）：

- N² 响应直转生成可用；
- `extract_usage_any` 提供准确的 usage 数据给 logging；
- logging 路径正确计算 cost_micro，非流不再静默白嫖。

迁移：
- 删除 `gw-core::error::codes::GW_REQUEST_NONSTREAM_NOT_SUPPORTED`（标 `#[deprecated]` 一个版本后真删）；
- CLAUDE.md 删除「Things that are intentional, not bugs」里那条；
- 测试 `e2e_failure_modes.rs` 若有该错误码 expectations → 改为非流走完整路径出 200。

### ADR-S4-v2-2 响应侧 schema 扩展

```rust
// gw-core::schema::response

#[ir_schema(target = "response")]
#[non_exhaustive]
pub struct ResponseSchema {
    pub id: SchemaField<String>,
    pub model: SchemaField<String>,
    pub messages: SchemaField<Vec<MessageSchema>>,  // 与请求侧复用
    pub finish_reason: SchemaField<Option<FinishReasonSchema>>,
    pub finish_reason_raw: SchemaField<Option<String>>,  // 原 wire 字符串，跨协议保留
    pub usage: SchemaField<Option<UsageSnapshotSchema>>,
    pub metadata: SchemaField<MetadataSchema>,
}

#[ir_schema(target = "usage_snapshot")]
#[non_exhaustive]
pub struct UsageSnapshotSchema {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
    pub thinking_tokens: Option<u32>,
    pub multimodal_input_tokens: Option<u32>,
}

#[ir_schema(target = "finish_reason")]
#[non_exhaustive]
pub enum FinishReasonSchema {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other,
}
```

`finish_reason_raw` 是新加字段——跨协议时原 wire finish_reason 字符串透传，避免"`stop_sequence` → `Stop` 后再回不去"的信息丢失。

### ADR-S4-v2-3 三家响应侧 wire 形状

形态与 v1 相同，去 `#[derive(IrConvert)]`：

```rust
// crates/gw-codec/src/openai/wire.rs（追加响应侧）

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiResponse<'a> {
    #[ir(map = "id")]
    pub id: Cow<'a, str>,

    #[ir(skip)]
    pub object: Cow<'a, str>,  // "chat.completion"

    #[ir(skip)]
    pub created: u64,

    #[ir(map = "model")]
    pub model: Cow<'a, str>,

    #[ir(map = "messages", via = "openai_choices_to_messages")]
    pub choices: Vec<OpenAiChoice<'a>>,

    #[ir(map = "usage", via = "openai_usage")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAiUsage>,

    #[ir(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<Cow<'a, str>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiChoice<'a> {
    #[ir(skip)]
    pub index: u32,
    pub message: OpenAiMessage<'a>,  // 复用 S2 请求侧 enum
    #[ir(map = "finish_reason", via = "openai_finish_reason")]
    pub finish_reason: Option<Cow<'a, str>>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct OpenAiUsage {
    #[ir(map = "usage.input_tokens")]
    pub prompt_tokens: u32,
    #[ir(map = "usage.output_tokens")]
    pub completion_tokens: u32,
    #[ir(skip)]
    pub total_tokens: u32,
    #[ir(map = "usage", via = "openai_prompt_details")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct OpenAiPromptTokensDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
}
```

Anthropic / Gemini 响应同理，形状与 v1 §ADR-S4-2 一致；详见原 spec。**注意**：响应 wire 复用 S2 请求侧的 MessageBlock 等 enum，所以多模态 / tools / thinking 自动支持。

### ADR-S4-v2-4 N² 响应直转

矩阵宏在 `gw-codec/src/matrix.rs` 调：

```rust
wire_convert_matrix! {
    schema: ResponseSchema,
    wires: [
        openai::wire::OpenAiResponse,
        anthropic::wire::AnthropicResponse,
        gemini::wire::GeminiResponse,
    ],
}
```

codegen 输出：

- 9 个 `impl From<A> for B`（含 3 个同协议恒等）；
- 9 个 `cross::convert_response_<src>_<dst>` 特化（如果 via 模块提供，优先用；否则 codegen 退化）；
- 每个 schema 字段对应的语义 trait（响应侧主要是 read trait，如 `HasFinishReason`）；
- `WireAnyResponse` enum。

via 模块跨 wire 特化（举例）：

```rust
// crates/gw-codec/src/openai/via.rs（响应侧）
pub mod openai_choices_to_messages {
    pub(crate) fn to_schema<'a>(src: &Vec<OpenAiChoice<'a>>) -> impl AsSchemaMessages<'a>;
    pub(crate) fn from_schema<'a>(src: impl AsSchemaMessages<'a>) -> Vec<OpenAiChoice<'a>>;
    pub(crate) mod cross {
        pub(crate) fn to_anthropic<'a>(src: Vec<OpenAiChoice<'a>>) -> Vec<AnthropicContentBlock<'a>>;
        pub(crate) fn to_gemini<'a>(src: Vec<OpenAiChoice<'a>>) -> Vec<GeminiCandidate<'a>>;
    }
}
```

### ADR-S4-v2-5 UsageSnapshot 提取函数

不让 schema 在 runtime 实例化，billing 不能持有 ResponseSchema；改提供一个轻量 POD：

```rust
// crates/gw-codec/src/usage.rs

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageSnapshot {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
    pub thinking_tokens: Option<u32>,
    pub multimodal_input_tokens: Option<u32>,
}

/// 从任意 wire 响应中提取 usage。codegen 派生 N 个 impl。
pub trait ExtractUsage {
    fn extract_usage(&self) -> Option<UsageSnapshot>;
}

impl ExtractUsage for OpenAiResponse<'_>     { /* codegen */ }
impl ExtractUsage for AnthropicResponse<'_>  { /* codegen */ }
impl ExtractUsage for GeminiResponse<'_>     { /* codegen */ }

/// _any 分发
pub fn extract_usage_any(wire: &WireAnyResponse) -> Option<UsageSnapshot> {
    match wire {
        WireAnyResponse::OpenAi(r) => r.extract_usage(),
        WireAnyResponse::Anthropic(r) => r.extract_usage(),
        WireAnyResponse::Gemini(r) => r.extract_usage(),
    }
}
```

billing 层（gw-billing，参 refactor plan）调 `extract_usage_any` 拿 POD → 计算 cost_micro。`UsageSnapshot` 是 32 字节左右的 Copy 类型，无堆分配。

`extract_usage` 由 schema 字段的 codegen 自动派生——它在编译期已经知道每个 wire 的 usage 字段路径与映射。

### ADR-S4-v2-6 finish_reason 跨协议映射

参 v1 §ADR-S4-3，映射表保留：

| OpenAI | Anthropic | Gemini | schema FinishReasonSchema |
|---|---|---|---|
| `stop` | `end_turn` | `STOP` | `Stop` |
| `length` | `max_tokens` | `MAX_TOKENS` | `Length` |
| `tool_calls` | `tool_use` | n/a | `ToolCalls` |
| `content_filter` | `refusal` | `SAFETY` | `ContentFilter` |
| - | `pause_turn` | - | `Other` |

映射放 `crates/gw-codec/src/common/finish_reason.rs`，被三家 via 模块共用。

跨协议直转时同步写两个字段：`finish_reason: Option<FinishReasonSchema>` + `finish_reason_raw: Option<String>`（原 wire 字符串透传）。

### ADR-S4-v2-7 跨协议响应矩阵

| 客户端 X \ 上游 Y | OpenAI | Anthropic | Gemini |
|---|---|---|---|
| OpenAI | passthrough | full re-render | full re-render |
| Anthropic | full re-render | passthrough | full re-render |
| Gemini | full re-render | full re-render | passthrough |

全量对称，覆盖所有 ContentPart 类型（含多模态输出、tool_use、thinking）。

**降级规则**（目标协议不支持某 ContentPart）：

- thinking → 目标不支持 thinking（OpenAI 非 o-series）：丢弃，with `tracing::warn!`，counter `gw_codec_thinking_drop_cross_protocol_total`；
- image output → 目标不支持图像输出（少见）：丢弃 + warn；
- tool_use → 目标必须支持（三家都支持），不允许降级；丢失则 `ErrorIR::GW_GATEWAY_INTERNAL` 短路。

**signature 跨协议警告**：Anthropic/Gemini 的 thinking signature 是各自上游验签的——跨协议时 signature 在目标侧无意义，必然丢失。本 spec 不处理"跨协议后回路再次请求"——参 v1 §ADR-S4-5 同款论述。

### ADR-S4-v2-8 logging 路径接入

撤回 nonstream 短路后，`gw-proxy::filters::logging.rs` 必须正确处理非流响应：

```rust
// gw-proxy::filters::response.rs（伪代码，response_body_filter EOS）
let upstream_wire = WireAnyResponse::from_protocol_kind(
    upstream_protocol,
    response_body_bytes,
)?;

let client_wire = if upstream_protocol == client_protocol {
    upstream_wire  // passthrough
} else {
    upstream_wire.convert_to(client_protocol)  // N² 直转
};

let usage_snapshot = extract_usage_any(&client_wire);
ctx.usage = usage_snapshot;

let response_bytes = client_wire.render(&mut buf)?;
// ...
```

logging 用 `ctx.usage`（POD）计费，与 wire 形态解耦。

新增 metric：`gw_codec_parse_response_total{protocol, result}`、`gw_codec_cross_protocol_response_total{src, dst}`、`gw_codec_extract_usage_total{protocol, has_usage}`。

### ADR-S4-v2-9 大响应体策略

非流响应通常 < 500KB；大响应体（视频 URL、长 JSON 输出）一次性 parse 内存翻倍。

- v1 不做 streaming parse；
- 阈值 1MB 加 warn metric `gw_codec_large_response_total`；
- 超过 5MB 拒收（`GW_RESPONSE_TOO_LARGE` 502）——保护内存。

### ADR-S4-v2-10 性能预算

| 路径 | 预算（P50） |
|---|---|
| Same-protocol passthrough（response，无 mutation） | < 200 ns（Cow Borrow + parse_response 仅做最小验证） |
| Cross-protocol（response） | < 8 µs（一次 deserialize + N² 直转 + 一次 serialize） |
| `extract_usage_any` | < 50 ns（字段 copy） |

新增 benches：
- `gw-codec/benches/response_n2.rs`；
- `gw-codec/benches/extract_usage.rs`。

## 3. 实施大纲

依赖：S1 v2 + S2 v2 全 land；S3 v2 并行。

1. schema 响应侧（`response.rs` / `usage.rs` / `finish_reason.rs`）+ 注解；
2. 三家响应 wire 文件（已在 S2 v1 设计内）：去 `#[derive(IrConvert)]`，加 `#[ir(...)]`；
3. 三家 via 模块响应侧 + `cross::to_X` 特化；
4. `gw-codec/src/matrix.rs` 增加响应 schema 矩阵调用；
5. `extract_usage_any` codegen 派生 + `UsageSnapshot` POD；
6. `gw-proxy` 改造：
   - 删除 nonstream 400 短路（`filters/request.rs:233-249`）；
   - `filters/response.rs` EOS 路径调 `WireAnyResponse::from_protocol_kind` → `extract_usage_any` → 渲染回客户端 wire；
   - CLAUDE.md 删除「intentional」那条；
7. 测试矩阵：
   - 9 对响应 roundtrip（每对至少 (a) 纯文本、(b) 多模态输出、(c) tool_use 输出、(d) thinking + signature 输出）；
   - extract_usage_any 在 6 个上游 + 多种 usage 组合下的精度；
   - 大响应体阈值告警；
   - logging 路径非流计费正确性。

## 4. 未决问题

1. **`OpenAi prompt_tokens_details` 与 Anthropic `cache_creation`**：OpenAI 没有 cache_creation 字段。Anthropic→OpenAI 时 cache_creation_input_tokens 应丢弃还是合并到 prompt_tokens？倾向 **schema 保留 cache_creation 给 billing 用，wire 输出按目标协议字段语义**（OpenAI wire 渲染时丢弃 cache_creation 字段）。
2. **Gemini `thoughts_token_count` 跨协议落点**：跨到 OpenAI 客户端时塞到 `completion_tokens_details.reasoning_tokens`（o-series 字段）。需要新增 OpenAI 响应 wire 字段。本 spec 加上。
3. **响应渲染回路下的 thinking signature**：跨协议必然丢失。是否在 schema 上加 `signature_origin_protocol: Option<ProtocolKind>` 帮 gateway 在请求侧识别"signature 来源协议 ≠ 上游协议 → 清理"？倾向加，但不阻塞 S4 land；可推到 adapter / 错误恢复 spec。
4. **大响应体（>1MB）的 streaming parse**：v1 推后；超过 5MB 拒收。但视频 URL 响应可能 30MB（base64 video）——是否要给 video 类响应特殊路径？倾向 video 走 task worker（参 task-worker spec），不走同步响应。
5. **CLAUDE.md 同 PR 修订**：撤回 nonstream 短路 = 删除「Things that are intentional」里那条 + 更新 README v1 limitations。
6. **跨协议 `tool_calls` 的 id 透传**：OpenAI `call_xxx`、Anthropic `toolu_xxx`、Gemini index。schema 保留原值（不重写格式）；客户端 SDK 必须容忍任意 id 字符串。需要在 codec roundtrip 测试覆盖"tool_call id 跨协议不变"。
7. **`extract_usage_any` 是否在 wire 包含部分 usage 时仍返回 Some**：例如 OpenAI 流式响应在 stream_options.include_usage 关闭时不发 usage——`extract_usage_any` 返回 None 还是部分 None？倾向"任一非 None 字段就返 Some（其他保 None）"，billing 自己处理 None 字段（保守计费）。

---

> 桶 1 codec 四份 spec 全 v2 完成。形态：IR 仅编译期 schema → wire→wire N² 直转 → mutation 与 usage 提取走 enum-dispatch helper → 不再有运行时 IR 实例。这是用户原话「编译期 A→IR→B 转为 A→B」的完整兑现。
