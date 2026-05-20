# Codec S2 — Wire 多模态升级（C1 + C2 + C3）（v2026-05-19，v2 重写）

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft（v2 重写，承接 S1 v2）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md)（**硬前置**：编译期 schema + 矩阵宏 + 语义 trait）
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 1 C1/C2/C3）
- **范围**：三家 wire（OpenAI / Anthropic / Gemini）请求侧形状升级到 array-of-blocks；tools / tool_use / tool_result 一等公民；thinking + signature 上 wire；wire 标 `#[ir(...)]` 注解供 S1 矩阵宏读；为 schema 字段实现动作型 trait（per-wire 手写）。
- **不在范围**：响应侧 wire（→ S4）、流式 wire（→ S3）。

> **v2 重写说明**：v1 假定每个 wire 自带 `#[derive(IrConvert)]` derive。v2 撤回——derive 不再存在；wire 只标 `#[ir(...)]` 字段注解，由 S1 v2 的 `wire_convert_matrix!` 集中宏读取。本 spec 形状本身不变，只是 derive 标记被注解标记替代，新增"动作型 trait per-wire 手写"小节。

---

## 1. 背景

S1 v2 落地后，**新增字段或新增协议的工作量从"改三处 codec"降到"加一处 enum 形状 + 一处 ir 注解"**。本 spec 是 S1 落地后的首次大规模"使用 codegen 能力"——把三家请求侧 wire 升级到 Aether internal 等价表达力。

## 2. 设计决策

### ADR-S2-v2-1 schema 扩字段（请求侧）

在 `gw-core::schema::request` 增补（v1 已规划，v2 保留）：

```rust
#[ir_schema(target = "request")]
#[non_exhaustive]
pub struct RequestSchema {
    pub model: SchemaField<String>,
    pub max_tokens: SchemaField<Option<u32>>,
    pub temperature: SchemaField<Option<f32>>,
    pub top_p: SchemaField<Option<f32>>,
    pub top_k: SchemaField<Option<u32>>,
    pub presence_penalty: SchemaField<Option<f32>>,
    pub frequency_penalty: SchemaField<Option<f32>>,
    pub seed: SchemaField<Option<u64>>,
    pub stop: SchemaField<Option<Vec<String>>>,
    pub stream: SchemaField<bool>,
    pub messages: SchemaField<Vec<MessageSchema>>,
    pub tools: SchemaField<Option<Vec<ToolSchema>>>,
    pub tool_choice: SchemaField<Option<ToolChoiceSchema>>,
    pub response_format: SchemaField<Option<ResponseFormatSchema>>,
    pub thinking_config: SchemaField<Option<ThinkingConfigSchema>>,
    pub cache_control: SchemaField<Option<CacheControlSchema>>,
    pub metadata: SchemaField<MetadataSchema>,
    pub extensions: SchemaField<HashMap<String, RawValue>>,
}

#[ir_schema(target = "message")]
#[non_exhaustive]
pub enum MessageSchema {
    System { content: ContentSchema },
    User { content: ContentSchema },
    Assistant { content: ContentSchema, tool_calls: Option<Vec<ToolCallSchema>> },
    Tool { tool_call_id: String, content: ContentSchema },
}

#[ir_schema(target = "content")]
#[non_exhaustive]
pub enum ContentSchema {
    Text(String),
    Parts(Vec<ContentPartSchema>),
}

#[ir_schema(target = "content_part")]
#[non_exhaustive]
pub enum ContentPartSchema {
    Text { text: String },
    Image { source: MediaSourceSchema },
    Audio { source: MediaSourceSchema },
    File { source: MediaSourceSchema },
    Thinking { text: String, signature: Option<String> },
    ToolUse { id: String, name: String, arguments: RawValue },
    ToolResult { tool_use_id: String, content: ContentSchema, is_error: Option<bool> },
}

#[ir_schema(target = "thinking_config")]
#[non_exhaustive]
pub struct ThinkingConfigSchema {
    pub kind: ThinkingKindSchema,  // Enabled | Disabled
    pub budget_tokens: Option<u32>,
}
```

撤回 v1 的 `MetadataIR::anthropic_thinking`，迁移到 `RequestSchema::thinking_config`。

### ADR-S2-v2-2 OpenAI wire 形状（不变 vs S2 v1）

形状基本同 v1，只是去除 `#[derive(IrConvert)]`：

```rust
// crates/gw-codec/src/openai/wire.rs

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiRequest<'a> {
    #[ir(map = "model")]
    pub model: Cow<'a, str>,

    #[ir(map = "messages", via = "openai_messages")]
    pub messages: Vec<OpenAiMessage<'a>>,

    #[ir(map = "stream", default = "false")]
    #[serde(default)]
    pub stream: bool,

    #[ir(map = "max_tokens")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    #[ir(map = "temperature")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[ir(map = "top_p")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    #[ir(map = "stop")]
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<Cow<'a, str>>>,

    #[ir(map = "tools", via = "openai_tools")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAiTool<'a>>>,

    #[ir(map = "tool_choice", via = "openai_tool_choice")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<OpenAiToolChoice<'a>>,

    #[ir(map = "response_format", via = "openai_response_format")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<OpenAiResponseFormat<'a>>,

    #[ir(map = "metadata.openai_logit_bias")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logit_bias: Option<HashMap<String, f32>>,

    #[ir(extensions)]
    #[serde(flatten)]
    pub extra: HashMap<String, Box<RawValue>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
#[ir(enum_tag = "role")]
pub enum OpenAiMessage<'a> {
    System { content: OpenAiContent<'a> },
    User { content: OpenAiContent<'a> },
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<OpenAiContent<'a>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<OpenAiToolCall<'a>>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<Cow<'a, str>>,  // o-series thinking
    },
    Tool {
        tool_call_id: Cow<'a, str>,
        content: OpenAiContent<'a>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
#[ir(untagged)]
pub enum OpenAiContent<'a> {
    Text(Cow<'a, str>),
    Blocks(Vec<OpenAiContentBlock<'a>>),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ir(enum_tag = "type")]
pub enum OpenAiContentBlock<'a> {
    Text { text: Cow<'a, str> },
    ImageUrl { image_url: OpenAiImageUrl<'a> },
    InputAudio { input_audio: OpenAiInputAudio<'a> },
    File { file: OpenAiFile<'a> },
}

// ... OpenAiImageUrl / OpenAiInputAudio / OpenAiFile / OpenAiTool / OpenAiToolCall / OpenAiToolChoice / OpenAiResponseFormat 略
```

### ADR-S2-v2-3 Anthropic wire 形状

形状同 v1，去 derive：

```rust
#[derive(Debug, Deserialize, Serialize)]
pub struct AnthropicRequest<'a> {
    #[ir(map = "model")]
    pub model: Cow<'a, str>,

    #[ir(map = "messages", via = "anthropic_messages")]
    pub messages: Vec<AnthropicMessage<'a>>,

    /// Anthropic 把 system 放顶级字段；codegen 通过 via 把它合并/拆分 messages
    #[ir(map = "messages", via = "anthropic_system_into_messages")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<AnthropicSystem<'a>>,

    #[ir(map = "max_tokens")]
    pub max_tokens: u32,

    #[ir(map = "temperature")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[ir(map = "stream", default = "false")]
    #[serde(default)]
    pub stream: bool,

    #[ir(map = "tools", via = "anthropic_tools")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool<'a>>>,

    #[ir(map = "tool_choice", via = "anthropic_tool_choice")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<AnthropicToolChoice<'a>>,

    #[ir(map = "thinking_config", via = "anthropic_thinking_config")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinkingConfig>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
#[ir(untagged)]
pub enum AnthropicSystem<'a> {
    Text(Cow<'a, str>),
    Blocks(Vec<AnthropicSystemBlock<'a>>),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
#[ir(enum_tag = "role")]
pub enum AnthropicMessage<'a> {
    User { content: AnthropicContent<'a> },
    Assistant { content: AnthropicContent<'a> },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
#[ir(untagged)]
pub enum AnthropicContent<'a> {
    Text(Cow<'a, str>),
    Blocks(Vec<AnthropicContentBlock<'a>>),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ir(enum_tag = "type")]
pub enum AnthropicContentBlock<'a> {
    Text {
        text: Cow<'a, str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl>,
    },
    Image { source: AnthropicImageSource<'a> },
    Thinking {
        thinking: Cow<'a, str>,
        signature: Cow<'a, str>,
    },
    RedactedThinking { data: Cow<'a, str> },
    ToolUse {
        id: Cow<'a, str>,
        name: Cow<'a, str>,
        input: Box<RawValue>,
    },
    ToolResult {
        tool_use_id: Cow<'a, str>,
        content: AnthropicToolResultContent<'a>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}
```

注意 `system` 与 `messages` 都 `map = "messages"`——这是 `anthropic_system_into_messages` via 函数职责：把顶级 system 字段合并/拆分到 schema 的 messages 列表第一项。

### ADR-S2-v2-4 Gemini wire 形状

形状同 v1：

```rust
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiRequest<'a> {
    #[ir(map = "model")]
    #[serde(skip)]
    pub model: Cow<'a, str>,

    #[ir(map = "messages", via = "gemini_contents")]
    pub contents: Vec<GeminiContent<'a>>,

    #[ir(map = "messages", via = "gemini_system_into_messages")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GeminiSystemInstruction<'a>>,

    #[ir(map = "generation_config", via = "gemini_generation_config")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GeminiGenerationConfig>,

    #[ir(map = "tools", via = "gemini_tools")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GeminiTool<'a>>>,

    #[ir(map = "tool_choice", via = "gemini_tool_config")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<GeminiToolConfig<'a>>,

    #[ir(map = "metadata.gemini_safety_settings", via = "gemini_safety")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_settings: Option<Vec<GeminiSafetySettingWire<'a>>>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiPart<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<GeminiInlineData<'a>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<GeminiFileData<'a>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<GeminiFunctionCall<'a>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_response: Option<GeminiFunctionResponse<'a>>,
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub thought: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<Cow<'a, str>>,
}
```

GeminiPart 是"字段并存"形态而非 tagged enum；via 函数 `gemini_contents` 把 parts 解析/重组为 schema 的 ContentPart 序列。

### ADR-S2-v2-5 via 模块的两类函数

按 S1 v2 ADR-S1-v2-4 契约，每个 via 模块提供：

```rust
// 1. 落地到 schema 形态（参考用，用于 codegen 推导跨 wire 字段对应）
//    实际 codegen 输出会用 cross 特化绕过实例化 schema
pub(crate) fn to_schema<'a>(src: &Vec<OpenAiMessage<'a>>) -> impl AsSchemaMessages<'a>;
pub(crate) fn from_schema<'a>(src: impl AsSchemaMessages<'a>) -> Vec<OpenAiMessage<'a>>;

// 2. 跨 wire 特化（推荐写法；codegen 优先用）
pub(crate) mod cross {
    pub(crate) fn to_anthropic<'a>(src: Vec<OpenAiMessage<'a>>)
        -> (Vec<AnthropicMessage<'a>>, Option<AnthropicSystem<'a>>);
    pub(crate) fn to_gemini<'a>(src: Vec<OpenAiMessage<'a>>)
        -> (Vec<GeminiContent<'a>>, Option<GeminiSystemInstruction<'a>>);
}
```

为何同时保留 `to_schema/from_schema` 和 `cross::to_X`：

- **`to_schema/from_schema`**：人类可读的"协议 ↔ schema 单向映射"，便于 review/调试；
- **`cross::to_X`**：性能与正确性兼顾的"协议 A ↔ 协议 B 直转"，避免 schema 中转的字段过滤损失；
- codegen 默认用 cross；cross 不存在时退化到 `to_schema → from_schema`（带 warn）。

新增 codec 时，已有 codec 的 via 模块需要补 `cross::to_NewCodec` 函数——**这是新增 codec 的唯一外部接触面**。

### ADR-S2-v2-6 schema 上的"动作型 trait"声明

简单 getter/setter（max_tokens, temperature, top_p, stream, model 等）由 codegen 自动派生 `HasField` trait。

复杂 mutation 需要在 schema 上显式声明：

```rust
#[ir_schema(target = "request")]
pub struct RequestSchema {
    // ...
    #[ir_schema_op(trait = "HasSystemPrompt", methods = ["prepend_system", "get_system_text"])]
    pub messages: SchemaField<Vec<MessageSchema>>,

    #[ir_schema_op(trait = "HasToolList", methods = ["push_tool", "remove_tool", "list_tools"])]
    pub tools: SchemaField<Option<Vec<ToolSchema>>>,

    #[ir_schema_op(trait = "HasThinkingConfig", methods = ["set_thinking_kind", "set_thinking_budget"])]
    pub thinking_config: SchemaField<Option<ThinkingConfigSchema>>,
}
```

codegen 看到 `ir_schema_op` 后：

- 生成 `pub trait HasSystemPrompt { fn prepend_system(...); fn get_system_text(...); }`；
- 但 **不生成各 wire 的 impl**——这部分必须 per-wire 手写，因为复杂 mutation 的 wire-specific 实现五花八门。
- 为该 trait 生成 `apply_prepend_system_any(&mut WireAnyRequest, ...)` 分发 helper。

per-wire 手写位置：`crates/gw-codec/src/{openai,anthropic,gemini}/ops.rs`：

```rust
// crates/gw-codec/src/openai/ops.rs
impl HasSystemPrompt for OpenAiRequest<'_> {
    fn prepend_system(&mut self, text: &str) {
        self.messages.insert(0, OpenAiMessage::System {
            content: OpenAiContent::Text(Cow::Owned(text.to_owned())),
        });
    }
    fn get_system_text(&self) -> Option<Cow<'_, str>> {
        self.messages.first().and_then(|m| match m {
            OpenAiMessage::System { content: OpenAiContent::Text(t) } => Some(t.clone()),
            _ => None,
        })
    }
}

// crates/gw-codec/src/anthropic/ops.rs
impl HasSystemPrompt for AnthropicRequest<'_> {
    fn prepend_system(&mut self, text: &str) {
        match &mut self.system {
            None => self.system = Some(AnthropicSystem::Text(Cow::Owned(text.to_owned()))),
            Some(AnthropicSystem::Text(t)) => {
                *t = Cow::Owned(format!("{text}\n{t}"));
            }
            Some(AnthropicSystem::Blocks(_)) => {
                // 插入到 blocks 头部
            }
        }
    }
    fn get_system_text(&self) -> Option<Cow<'_, str>> {
        match &self.system {
            Some(AnthropicSystem::Text(t)) => Some(t.clone()),
            // ...
            _ => None,
        }
    }
}
```

### ADR-S2-v2-7 Wire ↔ schema 边界情况

| 情形 | 处理 |
|---|---|
| OpenAI 客户端发纯 string content | `OpenAiContent::Text` → schema `Content::Text` |
| OpenAI 客户端发 array content | `OpenAiContent::Blocks` → schema `Content::Parts` |
| Anthropic system 既可 string 也可 [block] | via 把 system 合并到 messages 头；目标 wire 若是 Anthropic 自身且 messages 头是单一 system Text → 渲染回 `system: Text`；否则展开为 `[block]` |
| Gemini Part 同时含 text + thought + thought_signature | 视为单 ContentPart::Thinking |
| OpenAI 客户端发图但下游 Anthropic 不支持图 | **不在 codec 处理**——capability mismatch 由 gw-routing 在选 key 阶段筛掉；codec 只负责协议 1:1 翻译 |
| schema 含 thinking block 但目标 wire 不支持 thinking | wire 的 cross::to_X 在直转时丢弃 thinking 块 + `tracing::warn!`；不报错 |
| Anthropic redacted_thinking | schema 用 `ContentPart::Thinking { signature: None }` + extensions raw；wire renderer 优先用 extensions raw 透传 |

### ADR-S2-v2-8 Mutations 自动派生

S1 v2 §ADR-S1-v2-8 已规定：codegen 为每个 schema 动作型 trait 自动生成对应的 Mutations bit。S2 落地后期望的 bit 集：

```rust
pub fn set_max_tokens(&mut self);
pub fn set_temperature(&mut self);
pub fn set_top_p(&mut self);
pub fn set_stream(&mut self);
pub fn set_model(&mut self);
pub fn set_messages(&mut self);
pub fn set_system_prompt(&mut self);
pub fn set_tools(&mut self);
pub fn set_tool_choice(&mut self);
pub fn set_thinking_config(&mut self);
pub fn set_metadata(&mut self);
pub fn set_cache_control(&mut self);
```

每个 `apply_*_any` 函数内部自动调对应 `set_*`。

## 3. 实施大纲

1. S1 v2 必须先 land。
2. schema 扩字段：`gw-core::schema` 新增 ContentPartSchema 内部变体（Thinking signature / ToolUse / ToolResult）+ ThinkingConfigSchema。
3. 三家 wire 文件按 §ADR-S2-v2-2 / 2-3 / 2-4 升级；删除 v1 的 `#[derive(IrConvert)]`；保留 serde derives；标 `#[ir(...)]`。
4. 每家 via 模块按 §ADR-S2-v2-5 写 `to_schema/from_schema` + `cross::to_*` 函数。
5. 每家 ops 模块按 §ADR-S2-v2-6 实现动作型 trait。
6. 在 `gw-codec/src/matrix.rs` 调 `wire_convert_matrix! { schema: RequestSchema, wires: [...] }`。
7. 测试：
   - 9 对（3×3）roundtrip 测试，每对包含多模态 / tools / thinking 用例；
   - 同协议无 mutation 走 passthrough 的 fast path 守 < 100 ns；
   - 跨协议直转守 < 5 µs；
   - 每个动作型 trait 的 `apply_*_any` 单测。

## 4. 未决问题

1. **OpenAI `tool_choice` 字符串变体**：`"auto" | "none" | "required" | {"type": "function", ...}` — `ir(untagged)` 标注 + via 手动 dispatch。需要确认 codegen 对 untagged enum 与 tagged enum 混合的支持。
2. **Anthropic `cache_control` 是否在所有 block 类型都支持**：v2 同 v1，只在 Text block 标，其余 block 加 cache_control 推到独立 spec。
3. **Gemini `function_call.args` 与 schema `ToolUse.arguments`**：`Box<RawValue>` ↔ `RawValue`，codegen 应能自动 deref/box。如不支持，via 函数手转一次。
4. **redacted_thinking 回路**：schema 用 `Thinking { signature: None }` 表达，wire renderer 优先用 extensions raw；codegen 需要识别"extensions 里特定 key 优先于结构化字段"——可能需要 schema 显式声明。
5. **OpenAI `reasoning_content` 在请求侧 vs 响应侧**：请求侧多轮可回填，响应侧 S4 才用。本 spec 只处理请求侧。
6. **Gemini v1 vs v1beta safety_settings 字段名差异**：本 spec 固定 v1；v1beta 兼容推后或走 adapter body patch。
7. **每家 ops 模块 trait impl 工作量**：动作型 trait（HasSystemPrompt / HasToolList / HasThinkingConfig）有 ~5 个；每家手写 ~5 个 impl = 15 个手写实现。可接受，但若再加新 trait 是 3 倍递增。考虑给某些标量动作（如 push_tool）codegen 提供默认实现，仅复杂动作（prepend_system 这种语义跨字段的）手写。
8. **extensions 桶里"协议特有 raw"的跨协议穿透**：OpenAI 客户端的 `extra` 里若有 Anthropic 看不懂的 key（如 `parallel_tool_calls`），跨协议时直接写到 Anthropic 的 extensions 桶？还是丢？倾向直接写，让上游自己 ignore。但需要 metric `gw_codec_unknown_extension_passthrough_total`。

---

> 本 spec 形态承接 S1 v2。S2 落地后 = 三家请求侧 wire 升级 + 9 对直转完整可用 + 业务 mutation 全部走 trait + WireAny。响应侧 / 流式各自 S4 / S3。
