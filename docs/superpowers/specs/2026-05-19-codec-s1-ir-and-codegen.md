# Codec S1 — 编译期 Schema + N² Wire 直转 Codegen（v2026-05-19，v2 重写）

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft（v2 重写，撤回 v1）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 1 C1/C2/C3/C5/C10）
  - [2026-05-18-llm-gateway-design.md](./2026-05-18-llm-gateway-design.md)（ADR-019）
- **范围**：把 IR 从 runtime 数据通路降级为**纯编译期 schema**；`gw-codec-macros` 用集中式 `wire_convert_matrix!` 宏一次性生成 N² 个 wire→wire 直转 + 语义 trait + WireAny enum + 业务 mutation 分发；运行时所有跨协议转换路径都是 wire→wire 直转，不再经过任何 IR 实例。
- **不在范围**：wire 字段形状（→ S2）、流式（→ S3）、响应 + cross-protocol（→ S4）。

> **v2 重写说明**：v1 把 IR 作为运行时数据通路（wire→IR→wire），并提议 `IrConvert/IrStreamConvert/IrErrorConvert` 三个 derive 兑现 2N 个映射。用户反馈："使用 IR 是为了通过 codegen，在编译期从 A→IR→B 转为 A→B" + "mutate 也应当生成直转代码"。v1 形态被彻底撤回。v2 形态：
> - IR struct 在 runtime 不再实例化（无公开构造函数），仅以 Rust struct + `#[ir_schema]` 注解形式存在供 codegen 读 schema；
> - codegen 生成 N²（不是 2N）个 wire→wire 直转；
> - 业务 mutation 写成"语义 trait" + `WireAny` enum 分发，绕过 IR；
> - 唯一 codegen 入口是顶层集中宏 `wire_convert_matrix!`；新增 codec = 在矩阵宏里加一个 ident，无需改其他文件。

---

## 1. 背景

v1 设计假设：跨协议路径 = `parse_request(wire_a) -> IR -> render_request(IR) -> wire_b`，IR 是热路径数据；codegen 生成的 derive 是 2N 个独立映射。

用户的反对：

1. **IR 在 runtime 路径上是冗余的**——每个跨协议请求至少多一次堆分配（IR struct）+ 两次字段复制。
2. **codegen 应当兑现"编译期消除 IR"的承诺**——一次性把 A→B 直转生成出来，让 runtime 路径变成 wire→wire 一步；IR 沦为 codegen 的元模型（schema），不再是数据。
3. **mutation 也应当编译期直转**——业务 filter 改 max_tokens、注 system 等操作，应当作用在具体 wire 上（通过语义 trait），不应"为了 mutation 临时落 IR"。

v2 形态把这三点贯彻到底。

## 2. 关键判断

### ADR-S1-v2-1 IR 是编译期 schema，不是 runtime 数据通路

`gw-core::ir` 改名为 `gw-core::schema`（命名上去除"中间表示"暗示），所有原 IR struct 保留为 Rust 类型形态，**但**：

- 字段保持 `pub`，供 macros 读取；
- **去除**所有公开构造函数（`Default` impl、`new`、struct literal 构造），加 `#[non_exhaustive]` 阻断 user 创建；
- 加 marker 属性 `#[ir_schema(target = "request")]`、`#[ir_schema(target = "response")]` 等，标注用途；
- 不实现 `Serialize / Deserialize`（runtime 永不实例化，序列化无意义）。

人类读 `schema/request.rs` 仍然能知道"网关心目中的请求规范"是什么样，**这是 IR 的最终价值**：文档 + codegen 输入，不是 runtime 数据。

例子：

```rust
// gw-core/src/schema/request.rs

#[ir_schema(target = "request")]
#[non_exhaustive]
pub struct RequestSchema {
    pub model: SchemaField<String>,
    pub max_tokens: SchemaField<Option<u32>>,
    pub temperature: SchemaField<Option<f32>>,
    pub messages: SchemaField<Vec<MessageSchema>>,
    pub tools: SchemaField<Option<Vec<ToolSchema>>>,
    pub thinking_config: SchemaField<Option<ThinkingConfigSchema>>,
    // ...
}

#[ir_schema(target = "content_part")]
#[non_exhaustive]
pub enum ContentPartSchema {
    Text { text: SchemaField<String> },
    Image { source: SchemaField<MediaSourceSchema> },
    Audio { source: SchemaField<MediaSourceSchema> },
    File { source: SchemaField<MediaSourceSchema> },
    Thinking {
        text: SchemaField<String>,
        signature: SchemaField<Option<String>>,
    },
    ToolUse {
        id: SchemaField<String>,
        name: SchemaField<String>,
        arguments: SchemaField<RawValue>,
    },
    ToolResult {
        tool_use_id: SchemaField<String>,
        content: SchemaField<ContentSchema>,
    },
}
```

`SchemaField<T>` 是个标记包装类型，**只在编译期被读取**，runtime 无构造，无字段值。它存在的唯一目的是让 macros 看到"这里是一个 T 类型的语义字段"。

### ADR-S1-v2-2 唯一 codegen 入口：`wire_convert_matrix!`

不在 wire 上标 `#[derive(IrConvert)]`，转而在 `gw-codec` 顶层集中宏调用：

```rust
// gw-codec/src/matrix.rs

wire_convert_matrix! {
    schema: RequestSchema,
    wires: [
        openai::wire::OpenAiRequest,
        anthropic::wire::AnthropicRequest,
        gemini::wire::GeminiRequest,
    ],
}

wire_convert_matrix! {
    schema: ResponseSchema,
    wires: [
        openai::wire::OpenAiResponse,
        anthropic::wire::AnthropicResponse,
        gemini::wire::GeminiResponse,
    ],
}

wire_convert_matrix! {
    schema: StreamChunkSchema,
    wires: [
        openai::wire::OpenAiStreamChunk,
        anthropic::wire::AnthropicStreamFrame,
        gemini::wire::GeminiStreamChunk,
    ],
    stream: true,  // 启用累积状态机生成
}
```

宏在编译期：
1. 读 schema struct（找 `#[ir_schema]` 标记的类型）；
2. 读每个 wire 上的 `#[ir(...)]` 字段注解（注解形态见 §ADR-S1-v2-3）；
3. 对每对 (A, B) 直转：对齐两边的 `#[ir(map = "...")]` 字段；
4. 输出 N² 个 `impl From<A> for B`（同协议是 `impl From<A> for A`，等价 `Clone`，宏跳过生成）；
5. 输出每个 schema 字段对应的语义 trait + N 个 impl；
6. 输出 `WireAny` enum（含 N 个 variant）+ N 个 `op_any` 分发 helper。

**关键属性**：新增 codec = 在 wires 列表里加一个 ident + 在 wire 文件里写 enum 形状 + 标 `#[ir(...)]` 注解 + 实现协议特定的 via 函数。**不改任何 filter、不改任何业务代码**。

### ADR-S1-v2-3 wire `#[ir(...)]` 注解契约

注解形态固化（与 v1 类似，但语义不同——v1 是"映射 IR 字段"，v2 是"对齐 schema 字段供宏推导跨 wire 字段对应"）：

| 属性 | 用途 | 示例 |
|---|---|---|
| `#[ir(map = "path")]` | 字段级。点路径定位 schema 字段（`metadata.openai_logit_bias`）；同一 schema 路径在不同 wire 上的多个字段，会被 codegen 视为"语义等价的跨 wire 对应" | `#[ir(map = "max_tokens")]` |
| `#[ir(via = "module_name")]` | 字段级。复杂字段需要 wire ↔ schema 的转换函数；多个 wire 各自指定独立 via | `#[ir(map = "messages", via = "openai_messages")]` |
| `#[ir(default = "literal_or_expr")]` | 字段级。目标 wire 缺该字段时的默认值 | `#[ir(map = "stream", default = "false")]` |
| `#[ir(extensions)]` | 字段级。该字段是 `HashMap` 透传桶；codegen 自动在直转时 clone 过去（key/value 类型必须能 borrow） | `#[serde(flatten)] #[ir(extensions)] pub extra: HashMap<...>` |
| `#[ir(skip)]` | 字段级。该字段不参与直转（仅 wire 自用，如 OpenAI `created` 这种时间戳） | `#[ir(skip)] pub created: u64` |
| `#[ir(only(in/out))]` | 字段级。该字段只在某方向参与（如响应侧的 `system_fingerprint` 没有 schema 字段，但需要原样回吐） | `#[ir(skip, only(out))]` |
| `#[ir(enum_tag = "type")]` | enum 级。tagged enum 走 serde tag，codegen 自动按 tag 分发对应转换 | `#[derive(Deserialize)] #[serde(tag = "type")] #[ir(enum_tag = "type")] pub enum OpenAiContentBlock` |
| `#[ir(untagged)]` | enum 级。serde untagged 形态；codegen 按字段存在性判定 | `#[serde(untagged)] #[ir(untagged)] pub enum OpenAiContent` |

**关键约束**：
- 注解错误（拼错路径、via 函数缺失、未知属性）必须 compile_error 指明 wire 名 + 字段名 + 期望形态；
- 同一 schema 路径在两个 wire 的 `via` 函数签名可不同（OpenAI 的 messages 是 Vec<OpenAiMessage>，Anthropic 的是 Vec<AnthropicMessage> + Option<AnthropicSystem>），codegen 自动衔接（详见 §ADR-S1-v2-4）。

### ADR-S1-v2-4 via 函数契约（跨 wire 推导）

每个 wire 字段标 `via = "module"` 时，该 module 必须提供：

```rust
mod openai_messages {
    /// 把本 wire 字段 → schema 形态（IR 实例化形态，由 codegen 决定要不要保留 runtime 形态）
    pub(crate) fn to_schema<'a>(src: &Vec<OpenAiMessage<'a>>) -> SchemaValue<'a>;
    /// 把 schema 形态 → 本 wire 字段
    pub(crate) fn from_schema<'a>(value: SchemaValue<'a>) -> Vec<OpenAiMessage<'a>>;
}
```

但因为 v2 要绕开 IR 实例化，**实际生成的是直转**：

codegen 看到 OpenAI wire `via = "openai_messages"` 与 Anthropic wire `via = "anthropic_messages"` 都标了 `map = "messages"`，自动尝试生成：

```rust
fn convert_openai_to_anthropic_messages<'a>(
    src: Vec<OpenAiMessage<'a>>
) -> (Vec<AnthropicMessage<'a>>, Option<AnthropicSystem<'a>>);
```

如果两个 via 模块都额外提供 `cross::<TargetWire>::convert(src) -> Target` 这个特化函数，codegen 直接用；如果没有，codegen 退化到 `to_schema → from_schema` 两步（仍是编译期生成的内联代码，runtime 不见 schema 实例），加一条 warn lint 提示"建议手写跨 wire 特化"。

**最常见的简单字段（max_tokens、temperature、stream）**：codegen 直接用类型 + map path 推导，不需要任何 via。

### ADR-S1-v2-5 语义 trait 自动派生

对 schema 每个字段，codegen 派生 `Has<FieldName>` trait + N 个 wire impl：

```rust
// codegen 输出：
pub trait HasMaxTokens {
    fn get_max_tokens(&self) -> Option<u32>;
    fn set_max_tokens(&mut self, v: Option<u32>);
}

impl HasMaxTokens for OpenAiRequest<'_>     { /* 字段直接访问 */ }
impl HasMaxTokens for AnthropicRequest<'_>  { ... }
impl HasMaxTokens for GeminiRequest<'_>     { ... }

pub trait HasSystemPrompt {
    fn prepend_system(&mut self, text: &str);
    fn get_system(&self) -> Option<Cow<'_, str>>;
}
// 这种"动作型"trait 不是简单 getter/setter——codegen 不能盲推；
// schema 字段需要标 `#[ir_schema(trait = "HasSystemPrompt", impl = "prepend_system")]` 显式声明
```

**结论**：codegen 自动派生**简单字段 trait**（GET/SET 标量、Option 标量）；**动作型 trait**（prepend_system, push_tool_call, replace_thinking）由 schema 显式标 trait 名，wire 各自手写 impl。两类共存。

业务 filter 写法：

```rust
// 简单 mutation —— 泛型适配所有 wire
fn cap_max_tokens<T: HasMaxTokens>(req: &mut T, cap: u32) {
    if let Some(curr) = req.get_max_tokens() {
        if curr > cap { req.set_max_tokens(Some(cap)); }
    }
}

// 复杂 mutation —— 同泛型
fn inject_system_prompt<T: HasSystemPrompt>(req: &mut T, text: &str) {
    req.prepend_system(text);
}

// filter 链入口（参 §ADR-S1-v2-6 WireAny + 分发 helper）
fn request_filter(ctx: &mut Ctx) {
    apply_cap_max_tokens_any(&mut ctx.wire, ctx.user.max_tokens_cap);
    apply_inject_system_any(&mut ctx.wire, &ctx.vendor.system_prefix);
}
```

### ADR-S1-v2-6 WireAny enum + 分发 helper

codegen 输出 `WireAny` enum 集中持有"当前请求的 wire 形态"：

```rust
pub enum WireAnyRequest<'a> {
    OpenAi(OpenAiRequest<'a>),
    Anthropic(AnthropicRequest<'a>),
    Gemini(GeminiRequest<'a>),
}

// 每个 schema 字段的简单 trait 自动派生 _any 分发函数
pub fn apply_cap_max_tokens_any(wire: &mut WireAnyRequest, cap: u32) {
    match wire {
        WireAnyRequest::OpenAi(r) => cap_max_tokens(r, cap),
        WireAnyRequest::Anthropic(r) => cap_max_tokens(r, cap),
        WireAnyRequest::Gemini(r) => cap_max_tokens(r, cap),
    }
}

// 动作型 trait 同理派生 _any
pub fn apply_inject_system_any(wire: &mut WireAnyRequest, text: &str) {
    match wire {
        WireAnyRequest::OpenAi(r) => inject_system_prompt(r, text),
        WireAnyRequest::Anthropic(r) => inject_system_prompt(r, text),
        WireAnyRequest::Gemini(r) => inject_system_prompt(r, text),
    }
}

// 跨协议直转的 _any 入口（运行时 vendor 选定后调）
pub fn convert_to_any<'a>(src: WireAnyRequest<'a>, target: ProtocolKind) -> WireAnyRequest<'a> {
    match (src, target) {
        (WireAnyRequest::OpenAi(r), ProtocolKind::Anthropic) => WireAnyRequest::Anthropic(r.into()),
        (WireAnyRequest::OpenAi(r), ProtocolKind::Gemini) => WireAnyRequest::Gemini(r.into()),
        (WireAnyRequest::Anthropic(r), ProtocolKind::OpenAi) => WireAnyRequest::OpenAi(r.into()),
        // ... N² 个分支
        (same, same_kind) => same,  // 同协议直接 return
    }
}
```

**新增 codec = 矩阵宏列表加 ident → codegen 自动补 enum variant + 分发 helper**。filter 代码完全不变。

### ADR-S1-v2-7 schema struct 在 runtime 真不实例化吗？

理论上不需要——所有路径都是 wire→wire。但有两个例外要明确：

- **detailed_log**（ADR-026 写入时脱敏的请求/响应快照）：是按 schema 字段写还是按 wire？倾向 wire（更贴近用户视角）；schema 在这条路径仍不出现。
- **billing usage 提取**：S4 决议用 `UsageSnapshot` POD（轻量结构体，不在 schema 体系内）；schema 仍不出现。

结论：runtime 路径上 schema struct **真**不实例化。schema 仅在 codegen 输入侧存在。

### ADR-S1-v2-8 Mutations 状态字段升级

`gw-core::ir::Mutations` 当前是简单 bit flag 跟踪。v2 形态下 mutation 直接作用于 wire，`Mutations` 仍有用——它决定 same-protocol passthrough：

```rust
pub struct Mutations {
    pub fields: u32,
}

impl Mutations {
    // codegen 为每个动作型 trait 生成对应的 bit:
    pub fn set_max_tokens(&mut self);
    pub fn set_messages(&mut self);
    pub fn set_thinking_config(&mut self);
    pub fn set_tools(&mut self);
    pub fn set_system_prompt(&mut self);
    pub fn set_metadata(&mut self);
}
```

`apply_*_any` 函数内部自动调对应的 `set_*`。passthrough 检查 `Mutations::is_empty()` 决定是否走 Cow::Borrowed 原路。

### ADR-S1-v2-9 性能预算

| 路径 | 预算（P50） | 备注 |
|---|---|---|
| Same-protocol passthrough（无 mutation） | < 100 ns | Cow::Borrowed，无 deserialize |
| Cross-protocol（无 mutation） | < 5 µs | 一次 serde parse + 一次 wire→wire 直转 + 一次 serde serialize |
| Cross-protocol（有 mutation） | < 8 µs | 上面 + N 个 mutation 应用（每个 < 100 ns） |
| 直转函数本身（不含 serde） | < 1 µs | benches 守住 |

新增 benches：
- `gw-codec/benches/wire_convert.rs` — N² 个直转的 micro bench；
- `gw-codec/benches/mutation_dispatch.rs` — `apply_cap_max_tokens_any` 等。

### ADR-S1-v2-10 错误处理（compile-time）

宏在以下情形发 compile_error：

- `#[ir_schema]` 标在 enum 但缺 `#[ir(enum_tag)]` 或 `#[ir(untagged)]`；
- wire 的 `#[ir(map = "path")]` 在 schema 上找不到路径（编译期 type-level check）；
- 两个 wire 在 schema 同一路径上声明类型不兼容（如 OpenAI 该字段是 `Option<u32>`、Anthropic 该字段是 `u32`），且未提供 cross 特化 via；
- via module 缺 `to_schema` / `from_schema`；
- 跨 wire 特化 via `cross::<Wire>::convert` 签名不匹配。

错误消息要带 wire ident + 字段 ident + 期望形态。

## 3. 实施大纲（详细 step 进 plan）

1. **schema 重构**：
   - `gw-core/src/ir.rs` 改名 `schema.rs`，加 `#[ir_schema]` 标注；
   - 撤回 Default impl / 公开构造函数；
   - 加 `ContentPartSchema::Thinking` + signature 字段（v1 已记录）；
   - 加 `ContentPartSchema::ToolUse / ToolResult`（S2 会用）；
   - `Mutations` 沿用，bit 位由 macros 自动派生。
2. **`gw-codec-macros` 实施**：
   - 撤回 `IrConvert / IrStreamConvert / IrErrorConvert` 三个 derive；
   - 新增单个 proc-macro `wire_convert_matrix!`；
   - 实现属性 parser + 跨 wire 字段对齐推导 + N² 直转生成 + 语义 trait 派生 + WireAny enum 生成。
3. **`gw-codec` 集中调用矩阵宏**：`gw-codec/src/matrix.rs` 列出所有 wire；编译产物里包含所有 N² 直转 + trait impl。
4. **三家 codec 改造**：
   - wire 文件保留，加 `#[ir(...)]` 注解；
   - 撤回原 `via.rs` 里的 From/TryFrom 手写；
   - 保留 via module（含 `cross::<Target>::convert` 特化函数）。
5. **filter 改造**：
   - 把直接操作 IR 的代码改为 `apply_*_any` 调用；
   - 删除 `gw-proxy::ctx` 里的 `request_ir` 字段，改为 `wire: WireAnyRequest<'a>`。
6. **基准 + 测试**：
   - 新增 N² 直转的 roundtrip 矩阵测试（9 对）；
   - 性能 bench 守 §ADR-S1-v2-9 预算。

## 4. 未决问题

1. **schema struct 在 runtime 完全 dead code 是否会被 rustc warning**：`SchemaField<T>` 类型若永不构造，rustc 会报 dead_code。需要在 schema 模块加 `#[allow(dead_code)]` 或 `cfg(not(...))` 抑制。倾向 `#[allow(dead_code)] mod schema;`。
2. **proc-macro 复杂度可控性**：矩阵宏要做"跨 wire 字段对齐推导 + via 特化解析 + enum 派生 + trait 派生"四件事。实测可能膨胀到数千行宏代码。是否要拆成多个内部宏（`matrix_dispatch`、`matrix_traits`、`matrix_conv`）？倾向拆，保持每段 < 500 行。
3. **extensions 桶的跨协议透传语义**：OpenAI 的 `extra: HashMap<String, Box<RawValue>>` 含未知字段。跨协议直转时这些字段应该：(a) 直接转给目标 wire 的 extra 桶；(b) 全部丢弃；(c) 按 key 名白名单。倾向 (a)，业务层自负其责。
4. **schema 字段的"主键"概念**：N² 直转中，OpenAI message id 与 Anthropic message id 是不同字符串域。schema 标 `#[ir_schema(stable_id = true)]` 让 codegen 知道这是个跨域稳定 id（保留原值）vs 临时 id（可重新分配）。倾向加这个属性。
5. **流式累积状态如何承载**（S3 主线，但 S1 派生 schema）：流式 ContentPart 的 signature 跨 chunk 拼接需要一个跨 chunk 状态机；S1 派生时是否要为 stream schema 加 `#[ir_schema(stream_accum = true)]` 标？倾向加，S3 用。
6. **新增 codec 时 codegen 失败的回滚成本**：若加了第 4 个 codec，3 个原 codec 的 via 函数没提供与新 codec 的特化，codegen 退化到 `to_schema → from_schema` 两步且 warn。允许部分 codec 间 cross 不完备的过渡期。需要在 CI 把 warn 升级 error 之前定义 cutoff。
7. **`WireAny` enum 的大小**：三个 variant 都是带 lifetime 的 wire struct，可能 100+ 字节。`#[non_exhaustive]` + Box 是否值得？倾向不 Box（栈分配仍快于 Box 一次），但加 const_assert 守住 size。

---

> 本 spec 重写后，S2/S3/S4 形态相应改变（S2 wire 注解、S3 流式 N² + 累积状态、S4 响应 N² + UsageSnapshot POD）。三个后续 spec 单独更新。
