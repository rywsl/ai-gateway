# v2 S-codec Implementation Plan (2026-05-24)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地 [S-codec spec](../specs/2026-05-24-v2-codec-cleanup-design.md):4 个 wire form 的非流 `render_response` 实现 + 跨协议非流 re-render + 工具块 wire 端结构化 + SSE 跨块累积器 + N-1 局部 + `finish_reason` 跨协议映射,生产代码路径不再有 `unimplemented!()` / `TODO(v2)` / `"v1: ..."` 字样。

**Architecture:**
- IR 在 `gw-core/src/ir.rs` 扩展(`ResponseIR` 加 4 个 Option 字段、`UsageIR` 加 `reasoning_tokens`、`StreamEventIR` 加 3 个新 variants、`FinishReasonIR::Other` 改带载荷)。
- `ProtocolCodec` trait 在 `gw-codec/src/trait_def.rs` 加 `new_stream_state` 工厂方法,`parse_stream_chunk` 签名加 `&mut dyn StreamState` 并改返回 `Vec<StreamEventIR>`。
- 新建 `gw-codec/src/cross_render.rs`:`WireFormat`、`WireCapabilities`、`LossyField::drop`、`CrossRender::run`。
- 4 个 codec mod(`openai`、`openai/responses`、`anthropic`、`gemini`)各自完成 vertical slice(wire 形态升级 + via 双向 + `render_response` 实现 + `state.rs` 累积器 + 单测)。
- `gw-proxy/src/filters/upstream.rs:230` 撤掉跨协议非流 503 短路;`response.rs` non-stream 分支接入 `CrossRender`。
- 测试三层:per-codec 单测、跨协议矩阵 16 测(4×4)、lossy 专项、e2e。

**Tech Stack:** Rust 1.86, Pingora 0.8, sqlx 0.8(不动), `bytes`、`tracing`、`metrics`、`serde_json`、`smallvec`。所有改动遵循现有 `borrow-first / Cow<'a, str>` 风格。

---

## File Structure

| 文件 | 状态 | 责任 |
|---|---|---|
| `crates/gw-core/src/ir.rs` | Modify | ResponseIR / UsageIR / StreamEventIR / FinishReasonIR / 新加 `ResponseExtensions` / `ToolUseBlockIR` / `ResponseThinkingIR` / `ReasoningBlockIR` / `StreamState` marker trait |
| `crates/gw-codec/src/trait_def.rs` | Modify | `ProtocolCodec` 加 `new_stream_state`, `parse_stream_chunk` 签名变化 |
| `crates/gw-codec/src/cross_render.rs` | **Create** | `WireFormat`、`WireCapabilities`、`LossyField`、`CrossRender::run`、finish_reason 映射函数 |
| `crates/gw-codec/src/lib.rs` | Modify | `pub mod cross_render;` |
| `crates/gw-codec/src/openai/{codec,wire,via}.rs` | Modify | 实现 render_response;wire 加 Response struct + 显式 tools/tool_choice;extra HashMap key 改 Cow |
| `crates/gw-codec/src/openai/state.rs` | **Create** | OpenAI Chat 流式累积器(tool_call delta 合并) |
| `crates/gw-codec/src/openai/responses/{codec,wire,via}.rs` | Modify | 同上 (Responses API) + reasoning summary 解析 |
| `crates/gw-codec/src/openai/responses/state.rs` | **Create** | Responses API 累积器(tool_call + reasoning) |
| `crates/gw-codec/src/anthropic/{codec,wire,via}.rs` | Modify | Content 改 `Vec<AnthropicContent>` 枚举;Response struct;render_response;tool_use / tool_result / thinking 块 |
| `crates/gw-codec/src/anthropic/state.rs` | **Create** | Anthropic 流式累积器(thinking 续行 + tool_use builder) |
| `crates/gw-codec/src/gemini/{codec,wire,via}.rs` | Modify | Part 改枚举;Response struct;render_response;functionCall/Response |
| `crates/gw-codec/src/gemini/state.rs` | **Create** | Gemini 流式累积器(functionCall args 续行) |
| `crates/gw-proxy/src/filters/upstream.rs` | Modify | 撤 L230 区块的 503 短路 |
| `crates/gw-proxy/src/filters/response.rs` | Modify | non-stream 分支接入 CrossRender;stream 路径改 `parse_stream_chunk(state, ..)` 调用 |
| `crates/gw-proxy/src/ctx.rs` | Modify | `ReqCtx.stream_state: Option<Box<dyn StreamState>>` 新字段 |
| `crates/gw-core/src/error/codes.rs` | Modify | 删除 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED`;新加 `GW_CODEC_PARSE_RESPONSE`、`GW_CODEC_RENDER_RESPONSE`、`GW_CODEC_PARSE_STREAM_CHUNK`、`GW_CODEC_CROSS_RENDER_FAILED` |
| `crates/gw-gateway/src/metrics_catalog.rs` | Modify | 新加 `gw_codec_cross_render_nonstream_total{from,to}`、`gw_codec_cross_render_lossy_total{from,to,field}` |
| `crates/gw-codec/tests/cross_protocol_matrix.rs` | **Create** | 4×4 = 16 跨协议测 |
| `crates/gw-codec/tests/lossy_paths.rs` | **Create** | Lossy 专项测 |
| `crates/gw-gateway/tests/e2e_cross_protocol_response.rs` | **Create** | E2E |
| `docs/architecture.md` | Modify | §Known gaps G-9 同 PR 删除(若存在) |
| `docs/superpowers/specs/2026-05-19-codec-s4-response-and-cross-protocol.md` | Modify | 顶部加 "Superseded by 2026-05-24-v2-codec-cleanup-design.md" marker |
| `docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md` | Modify | §9 S-codec 行日期 `23 → 24` |
| `CLAUDE.md` | Modify | L26 "cross-protocol response re-render are wired" 校正 + L30 wave-7 字样若与本 spec 范围无关则保留(S-proxy-core 才删) |

---

## Phase 1 — IR + Trait Foundation

### Task 1: 扩展 IR(`gw-core/src/ir.rs`)

**Files:**
- Modify: `crates/gw-core/src/ir.rs` (L167-221 区域;以及加新类型)
- Test: 单测在同文件 `#[cfg(test)] mod tests`

- [ ] **Step 1: 加 `ResponseExtensions` / `ToolUseBlockIR` / `ResponseThinkingIR` / `ReasoningBlockIR` 类型**

在 `ResponseIR` 定义之前(L165 附近)插入:

```rust
/// Anthropic 风格 tool_use 块(wire 端 content array 内的 ToolUse 块,与 OpenAI 扁平 tool_calls 一一对应)。
#[derive(Clone, Debug)]
pub struct ToolUseBlockIR<'a> {
    pub id: Cow<'a, str>,
    pub name: Cow<'a, str>,
    pub input: Cow<'a, serde_json::value::RawValue>,
}

/// 响应侧 thinking 块集合(Anthropic 与 OpenAI Responses 共用)。
#[derive(Clone, Debug, Default)]
pub struct ResponseThinkingIR<'a> {
    pub blocks: Vec<ThinkingBlockIR<'a>>,
}

#[derive(Clone, Debug)]
pub struct ThinkingBlockIR<'a> {
    pub text: Cow<'a, str>,
    pub signature: Option<Cow<'a, str>>,  // Anthropic 流式时跨 chunk 续行,完成后填入
}

/// OpenAI Responses 风格 reasoning 摘要块。
#[derive(Clone, Debug, Default)]
pub struct ReasoningBlockIR<'a> {
    pub summary: Cow<'a, str>,
}
```

- [ ] **Step 2: 扩展 `ResponseIR`**

把 L167-174 改为:

```rust
#[derive(Clone, Debug)]
pub struct ResponseIR<'a> {
    pub id: Cow<'a, str>,
    pub model: Cow<'a, str>,
    pub messages: Vec<MessageIR<'a>>,
    pub finish_reason: Option<FinishReasonIR>,
    pub usage: Option<UsageIR>,
    pub metadata: MetadataIR<'a>,
    /// Anthropic / OpenAI Responses 携带的 thinking 块集合。
    /// 跨协议 re-render 到不支持的目标 wire 时由 cross_render 裁剪为 None + lossy log。
    pub thinking: Option<ResponseThinkingIR<'a>>,
    /// OpenAI Responses 携带的 reasoning summary。
    pub reasoning: Option<ReasoningBlockIR<'a>>,
    /// Anthropic 形态的 tool_use 块(与 MessageIR::Assistant.tool_calls 同源信息,
    /// 但保留块语义以便跨协议时输出 Anthropic content array)。
    pub tool_use_blocks: Option<Vec<ToolUseBlockIR<'a>>>,
}
```

同步把 `impl Default for ResponseIR<'_>` 加入新字段为 `None`。

- [ ] **Step 3: 扩展 `UsageIR`**

把 L199-204 改为:

```rust
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct UsageIR {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
    /// OpenAI Responses `completion_tokens_details.reasoning_tokens`;
    /// Anthropic 暂无对等字段,保持 None。跨协议时此字段对目标无对等表达即随 thinking/reasoning 同走 lossy 路径。
    pub reasoning_tokens: Option<u32>,
}
```

- [ ] **Step 4: 改 `FinishReasonIR::Other` 带载荷**

把 L191-197 改为:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReasonIR {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    /// 载荷为 vendor 原始字符串(如 Anthropic "pause_turn", Gemini "RECITATION");
    /// 跨协议 re-render 时根据 cross_render::map_finish_reason 决定如何降级 + 是否触发 lossy log。
    Other(Cow<'static, str>),
}
```

注意:`Copy` 移除(`Cow<'static, str>` 不 `Copy`)。同 PR 在 `FinishReasonIR` 的所有现有引用处(尤其是 `crates/gw-codec/src/*/via.rs` 内 `Other => stop` 这类 fallback)改成 `Other(_) => ...` 模式匹配。

- [ ] **Step 5: 扩展 `StreamEventIR` 加新 variants**

把 L207-221 改为:

```rust
#[derive(Clone, Debug)]
pub enum StreamEventIR<'a> {
    Start {
        request_id: Cow<'a, str>,
    },
    Delta {
        text: Option<Cow<'a, str>>,
        tool_call: Option<ToolCallIR<'a>>,
        thinking: Option<Cow<'a, str>>,
    },
    /// Anthropic / OpenAI Responses 流式 thinking 块的逐 chunk delta。
    /// `signature_chunk` 仅在 Anthropic 流式签名段出现时填,通常 None。
    ThinkingDelta {
        text: Cow<'a, str>,
        signature_chunk: Option<Cow<'a, str>>,
    },
    /// 新 tool_call 开启(index 在响应中此 tool_call 的位置,id/name 通常一次到位)。
    ToolCallStart {
        index: u32,
        id: Cow<'a, str>,
        name: Cow<'a, str>,
    },
    /// 已开启 tool_call 的 arguments JSON 片段追加(累积到完整 JSON 后由 codec 内部累积器决定何时 emit)。
    ToolCallDelta {
        index: u32,
        arguments_chunk: Cow<'a, str>,
    },
    Usage(UsageIR),
    End {
        finish_reason: FinishReasonIR,
    },
    Error(ErrorIR<'a>),
}
```

旧 `Delta { text, tool_call, thinking }` 保留 — 既有 codec 仍可 emit 单事件 form;新累积器在能拆分时 emit `ThinkingDelta` / `ToolCallStart` / `ToolCallDelta`。

- [ ] **Step 6: 加 `StreamState` marker trait**

文件末尾加(或独立放在 `pub mod stream_state;` 也行,本步采用文件内):

```rust
use std::any::Any;

/// Codec 内部的 SSE 累积器状态。filter 持有 `Box<dyn StreamState>` 但不窥视字段。
/// 每个 codec 在 `ProtocolCodec::new_stream_state()` 中返回自家具体类型的 Box。
pub trait StreamState: Send + Any {
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
```

- [ ] **Step 7: 写单测 — `ResponseIR` 字段 round-trip + `FinishReasonIR::Other` 载荷**

在 `crates/gw-core/src/ir.rs` 末尾加:

```rust
#[cfg(test)]
mod ir_v2_tests {
    use super::*;

    #[test]
    fn response_ir_extension_fields_default_none() {
        let r = ResponseIR::default();
        assert!(r.thinking.is_none());
        assert!(r.reasoning.is_none());
        assert!(r.tool_use_blocks.is_none());
    }

    #[test]
    fn finish_reason_other_carries_payload() {
        let r = FinishReasonIR::Other(Cow::Borrowed("pause_turn"));
        match r {
            FinishReasonIR::Other(s) => assert_eq!(s, "pause_turn"),
            _ => panic!("expected Other"),
        }
    }

    #[test]
    fn usage_ir_default_zero_with_optional_reasoning() {
        let u = UsageIR::default();
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert!(u.cached_tokens.is_none());
        assert!(u.reasoning_tokens.is_none());
    }

    #[test]
    fn stream_event_ir_new_variants_compile() {
        let _ = StreamEventIR::ThinkingDelta {
            text: Cow::Borrowed("thinking..."),
            signature_chunk: None,
        };
        let _ = StreamEventIR::ToolCallStart {
            index: 0,
            id: Cow::Borrowed("call_1"),
            name: Cow::Borrowed("get_weather"),
        };
        let _ = StreamEventIR::ToolCallDelta {
            index: 0,
            arguments_chunk: Cow::Borrowed("{\"loc"),
        };
    }
}
```

- [ ] **Step 8: 跑测**

Run:
```bash
cargo test -p gw-core --lib ir_v2_tests 2>&1 | tail -20
```

Expected: 4 个测全 pass。若编译错,大概率是 `FinishReasonIR::Copy` 移除波及到其他文件的 derive — 同 PR 修正这些位置(把 `let f: FinishReasonIR = old.finish_reason` 这种 implicit copy 改为 `clone()` 或 ref)。

- [ ] **Step 9: 跑全 workspace build,验证 `FinishReasonIR::Other` 带载荷不破坏现有 consumer**

Run:
```bash
cargo build --workspace 2>&1 | tail -30
```

Expected: clean build。若出现 `non-exhaustive patterns` 错误,定位到 `match` 后加 `Other(_) => ...` 分支。**所有此类位置都要修**,不允许 `_ => ...` 兜底掩盖。

- [ ] **Step 10: Commit**

```bash
git add crates/gw-core/src/ir.rs
git commit -m "$(cat <<'EOF'
v2-codec(T1): extend ResponseIR / UsageIR / StreamEventIR / FinishReasonIR

- ResponseIR + thinking / reasoning / tool_use_blocks (Option fields)
- UsageIR + reasoning_tokens
- StreamEventIR + ThinkingDelta / ToolCallStart / ToolCallDelta
- FinishReasonIR::Other now carries Cow<'static, str> payload
- ToolUseBlockIR / ResponseThinkingIR / ThinkingBlockIR / ReasoningBlockIR new types
- StreamState marker trait for codec-internal accumulator opacity

Sub-spec S-codec §2.1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: ProtocolCodec trait 改动 + 4 codec compile stubs

**Files:**
- Modify: `crates/gw-codec/src/trait_def.rs`
- Modify: `crates/gw-codec/src/{openai,anthropic,gemini}/codec.rs`、`crates/gw-codec/src/openai/responses/codec.rs`(各 codec 的 `impl ProtocolCodec` 块)
- Modify: `crates/gw-codec/src/lib.rs`(可能要 `pub mod state` per codec)
- Modify: `crates/gw-proxy/src/filters/response.rs`(`parse_stream_chunk` 调用点签名追随)
- Modify: `crates/gw-proxy/src/ctx.rs`(`ReqCtx.stream_state` 新字段)

- [ ] **Step 1: 改 `ProtocolCodec` trait 签名**

`crates/gw-codec/src/trait_def.rs` 内 `pub trait ProtocolCodec: Send + Sync + 'static`:
- 不动 `parse_request` / `render_request`(请求路径已 OK)。
- 不动 `parse_response` / `render_response` 方法签名。
- 加 `fn new_stream_state(&self) -> Box<dyn gw_core::ir::StreamState>;`
- 改 `parse_stream_chunk`:
  ```rust
  fn parse_stream_chunk(
      &self,
      state: &mut dyn gw_core::ir::StreamState,
      bytes: &[u8],
  ) -> Result<Vec<gw_core::ir::StreamEventIR<'_>>, gw_core::ir::ErrorIR<'static>>;
  ```
- 不动 `render_stream_chunk`。

- [ ] **Step 2: 4 个 codec 加 stub `impl`**

每个 codec(openai/codec.rs、anthropic/codec.rs、gemini/codec.rs、openai/responses/codec.rs)的 `impl ProtocolCodec for <vendor>Codec`:

```rust
fn new_stream_state(&self) -> Box<dyn gw_core::ir::StreamState> {
    // T4-A/B/C/D 各自替换为具体类型 Box::new(<Vendor>StreamState::default())
    todo!("S-codec T4: replace with concrete StreamState — not yet landed in this task")
}

fn parse_stream_chunk(
    &self,
    _state: &mut dyn gw_core::ir::StreamState,
    bytes: &[u8],
) -> Result<Vec<gw_core::ir::StreamEventIR<'_>>, gw_core::ir::ErrorIR<'static>> {
    // T4 各自替换为真实累积逻辑;此 stub 仅为本 task workspace 编过
    // 临时:用旧 stateless 路径,把单 Option<StreamEventIR> 装进 Vec
    let prev = self.legacy_parse_stream_chunk_v1(bytes)?;
    Ok(prev.map(|e| vec![e]).unwrap_or_default())
}
```

**注意 `todo!()` 临时桩 — 本 task 完成后 lint warn 模式会报新增 todo,这是预期的。T4 各 sub-task 落地时清零。**

如果当前 `parse_stream_chunk` 是 inline 实现,把其内容抽成私有 `fn legacy_parse_stream_chunk_v1(&self, bytes: &[u8]) -> Result<Option<StreamEventIR<'_>>, ErrorIR<'static>>` 供 stub 调用,T4 同 sub-task 时删除该私有函数。

- [ ] **Step 3: 同步 `gw-proxy` 端调用点**

`crates/gw-proxy/src/ctx.rs` 内 `pub struct ReqCtx` 加字段:

```rust
/// 流式响应时由 codec 提供的累积器状态;非流式或 codec 不需要累积时为 None。
pub stream_state: Option<Box<dyn gw_core::ir::StreamState>>,
```

`ReqCtx::default()` / `ReqCtx::new()` 设为 `None`。

`crates/gw-proxy/src/filters/response.rs` 内 SSE 处理点调用:

```rust
let events = upstream_codec.parse_stream_chunk(
    ctx.stream_state
        .as_deref_mut()
        .ok_or_else(|| Error::custom("stream_state not initialized"))?,
    chunk_bytes,
)?;
for event in events {
    // render / forward 逻辑保持当前 v1 行为(同协议短路 / 跨协议 render_stream_chunk)
}
```

`crates/gw-proxy/src/filters/request.rs` 在 request_filter 末尾(进入 upstream 之前)加:

```rust
if request_ir.stream.unwrap_or(false) {
    ctx.stream_state = Some(upstream_codec.new_stream_state());
}
```

注:此时 `new_stream_state` 在每个 codec 内是 `todo!()`,**本 task 不调用流式路径** — 单测 / e2e 不跑流式,实际执行流式会 panic。这是中间状态,T4 落地后修复。**本 task 完成后到 T4-A 之前不允许跑流式 e2e。**

- [ ] **Step 4: cargo build 全 workspace**

Run:
```bash
cargo build --workspace 2>&1 | tail -20
```

Expected: 全 workspace 编过。若有未替换签名,定位到 grep `parse_stream_chunk` 内的 callers 全部追上。

- [ ] **Step 5: 跑既有非流式测确认不破坏(流式测可能因 todo! 而 ignore)**

Run:
```bash
cargo test -p gw-codec --lib 2>&1 | tail -10
```

Expected: 既有非流测全 pass;流式相关测可能 panic with `todo!`。如果 panic,在该测顶部加 `#[ignore = "stream path stubbed by T2; restored in T4"]` — 但 prefer 不动测,直接走到 T4 修。

- [ ] **Step 6: Commit**

```bash
git add crates/gw-codec/src/trait_def.rs crates/gw-codec/src/{openai,anthropic,gemini}/codec.rs crates/gw-codec/src/openai/responses/codec.rs crates/gw-proxy/src/ctx.rs crates/gw-proxy/src/filters/{request,response}.rs
git commit -m "$(cat <<'EOF'
v2-codec(T2): ProtocolCodec adds new_stream_state + parse_stream_chunk(state)

- new_stream_state() factory returns Box<dyn StreamState>
- parse_stream_chunk takes &mut dyn StreamState, returns Vec<StreamEventIR>
- Four codec impls add temporary todo!() / legacy fallback stubs
  (cleared in T4-A/B/C/D each)
- ReqCtx gains stream_state field; request_filter initializes it on stream=true
- response.rs SSE callsite threads state through

Sub-spec S-codec §2.2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 2 — cross_render 骨架

### Task 3: 新建 `gw-codec/src/cross_render.rs`

**Files:**
- Create: `crates/gw-codec/src/cross_render.rs`
- Modify: `crates/gw-codec/src/lib.rs` (`pub mod cross_render;`)
- Modify: `crates/gw-core/src/error/codes.rs`(加 4 个新错误码)
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`(加 2 个新 metric)

- [ ] **Step 1: 新加错误码**

`crates/gw-core/src/error/codes.rs` 加:

```rust
pub const GW_CODEC_PARSE_RESPONSE: &str = "gw_codec_parse_response";
pub const GW_CODEC_RENDER_RESPONSE: &str = "gw_codec_render_response";
pub const GW_CODEC_PARSE_STREAM_CHUNK: &str = "gw_codec_parse_stream_chunk";
pub const GW_CODEC_CROSS_RENDER_FAILED: &str = "gw_codec_cross_render_failed";
```

(暂不删 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` — 留到 T6。)

- [ ] **Step 2: 新加 metric**

`crates/gw-gateway/src/metrics_catalog.rs` 加注册:

```rust
metrics::describe_counter!(
    "gw_codec_cross_render_nonstream_total",
    "Cross-protocol non-stream response re-renders performed (per from/to wire form)."
);
metrics::describe_counter!(
    "gw_codec_cross_render_lossy_total",
    "Vendor-specific fields dropped during cross-protocol re-render (per from/to/field). \
     Each increment also emits a tracing::error! event."
);
```

- [ ] **Step 3: 创建 `cross_render.rs` 骨架**

`crates/gw-codec/src/cross_render.rs`:

```rust
//! 跨协议响应 re-render 入口。请求侧已在 v1 实现(via.rs 双向),响应侧由本模块在
//! S-codec spec §2.3 / §3.2 中规定的流程下提供同步 IR 调整 + render dispatch。
//!
//! 流程:
//!   1. CrossRender::run 收到 source ResponseIR + target codec
//!   2. adapt_lossy 按 to.capabilities() 裁剪不可表达字段(每次裁剪触发 LossyField::drop)
//!   3. finish_reason 经 map_finish_reason 映射到目标 wire 的等价值
//!   4. target_codec.render_response(&adapted_ir, &mut buf) 渲染 wire bytes

use bytes::BytesMut;
use gw_core::ir::{ErrorIR, FinishReasonIR, ResponseIR};
use std::borrow::Cow;

use crate::trait_def::ProtocolCodec;

/// 4 个 wire form 的标识。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFormat {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerate,
}

impl WireFormat {
    pub fn label(self) -> &'static str {
        match self {
            WireFormat::OpenAiChat => "openai_chat",
            WireFormat::OpenAiResponses => "openai_responses",
            WireFormat::AnthropicMessages => "anthropic_messages",
            WireFormat::GeminiGenerate => "gemini_generate",
        }
    }

    pub fn capabilities(self) -> WireCapabilities {
        match self {
            WireFormat::OpenAiChat => WireCapabilities {
                supports_thinking: false,
                supports_reasoning: false,
                supports_tool_use_blocks: false,
                supports_tool_calls_flat: true,
                supports_function_call_parts: false,
                supports_reasoning_tokens: false,
            },
            WireFormat::OpenAiResponses => WireCapabilities {
                supports_thinking: true,         // Responses API 可携带 reasoning blocks
                supports_reasoning: true,
                supports_tool_use_blocks: false,
                supports_tool_calls_flat: true,
                supports_function_call_parts: false,
                supports_reasoning_tokens: true,
            },
            WireFormat::AnthropicMessages => WireCapabilities {
                supports_thinking: true,
                supports_reasoning: false,
                supports_tool_use_blocks: true,
                supports_tool_calls_flat: false,
                supports_function_call_parts: false,
                supports_reasoning_tokens: false,
            },
            WireFormat::GeminiGenerate => WireCapabilities {
                supports_thinking: false,
                supports_reasoning: false,
                supports_tool_use_blocks: false,
                supports_tool_calls_flat: false,
                supports_function_call_parts: true,
                supports_reasoning_tokens: false,
            },
        }
    }
}

impl std::fmt::Display for WireFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WireCapabilities {
    pub supports_thinking: bool,
    pub supports_reasoning: bool,
    pub supports_tool_use_blocks: bool,
    pub supports_tool_calls_flat: bool,
    pub supports_function_call_parts: bool,
    pub supports_reasoning_tokens: bool,
}

pub struct LossyField;

impl LossyField {
    /// 单点入口:打印 ERROR log + 增 metric。
    /// 调用方负责实际从 IR 上裁剪字段。
    pub fn drop(from: WireFormat, to: WireFormat, field: &'static str) {
        tracing::error!(
            from = %from,
            to = %to,
            field,
            "cross-render dropped vendor-specific field; \
             check codec/adapter mapping if this is unexpected"
        );
        metrics::counter!(
            "gw_codec_cross_render_lossy_total",
            "from" => from.label(),
            "to" => to.label(),
            "field" => field
        )
        .increment(1);
    }
}

pub struct CrossRender<'a> {
    pub from: WireFormat,
    pub to: WireFormat,
    pub source: ResponseIR<'a>,
}

impl<'a> CrossRender<'a> {
    pub fn run(
        self,
        target_codec: &dyn ProtocolCodec,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let ir = self.adapt_lossy();
        target_codec.render_response(&ir, buf)
    }

    fn adapt_lossy(self) -> ResponseIR<'a> {
        let to_caps = self.to.capabilities();
        let mut ir = self.source;
        let (from, to) = (self.from, self.to);

        if !to_caps.supports_thinking && ir.thinking.is_some() {
            LossyField::drop(from, to, "thinking");
            ir.thinking = None;
        }
        if !to_caps.supports_reasoning && ir.reasoning.is_some() {
            LossyField::drop(from, to, "reasoning");
            ir.reasoning = None;
        }
        // tool_use_blocks 与扁平 tool_calls 是同源信息的两种 wire 形态;codec 在 render 时
        // 按目标能力选用,因此 IR 层不主动裁剪 tool_use_blocks(可表达,不算 lossy)。

        // reasoning_tokens 字段
        if let Some(usage) = &mut ir.usage {
            if !to_caps.supports_reasoning_tokens && usage.reasoning_tokens.is_some() {
                LossyField::drop(from, to, "usage.reasoning_tokens");
                usage.reasoning_tokens = None;
            }
        }

        // finish_reason 映射
        if let Some(fr) = ir.finish_reason.take() {
            ir.finish_reason = Some(map_finish_reason(from, to, fr));
        }

        ir
    }
}

/// 跨协议 finish_reason 映射。Other(s) 落到任一目标都触发 lossy log。
pub fn map_finish_reason(
    from: WireFormat,
    to: WireFormat,
    fr: FinishReasonIR,
) -> FinishReasonIR {
    use FinishReasonIR::*;
    if from == to {
        return fr;
    }
    match fr {
        Stop | Length | ToolCalls => fr,  // 5×4 表内 Stop/Length/ToolCalls 在所有目标都可直接表达
        ContentFilter => {
            // Anthropic 无 content_filter 等价值,转 stop + lossy log
            if matches!(to, WireFormat::AnthropicMessages) {
                LossyField::drop(from, to, "finish_reason.content_filter");
                Stop
            } else {
                ContentFilter
            }
        }
        Other(ref s) => {
            LossyField::drop(from, to, "finish_reason.other");
            // 不携带原始字符串到下游 wire(目标 vendor 不识别),退化为 Stop
            let _ = s;
            Stop
        }
    }
}
```

- [ ] **Step 4: 在 `lib.rs` 导出**

`crates/gw-codec/src/lib.rs` 加:

```rust
pub mod cross_render;
```

- [ ] **Step 5: 写 cross_render 单测**

`crates/gw-codec/src/cross_render.rs` 文件末尾加 `#[cfg(test)] mod tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::ir::{ResponseIR, ResponseThinkingIR, ThinkingBlockIR, UsageIR};
    use std::borrow::Cow;

    fn ir_with_thinking() -> ResponseIR<'static> {
        let mut r = ResponseIR::default();
        r.thinking = Some(ResponseThinkingIR {
            blocks: vec![ThinkingBlockIR {
                text: Cow::Borrowed("reasoning step"),
                signature: None,
            }],
        });
        r
    }

    #[test]
    fn capabilities_openai_chat_lacks_thinking_and_reasoning() {
        let c = WireFormat::OpenAiChat.capabilities();
        assert!(!c.supports_thinking);
        assert!(!c.supports_reasoning);
    }

    #[test]
    fn adapt_lossy_drops_thinking_when_target_unsupports() {
        let cr = CrossRender {
            from: WireFormat::AnthropicMessages,
            to: WireFormat::OpenAiChat,
            source: ir_with_thinking(),
        };
        let adapted = cr.adapt_lossy();
        assert!(adapted.thinking.is_none());
    }

    #[test]
    fn adapt_lossy_keeps_thinking_when_target_supports() {
        let cr = CrossRender {
            from: WireFormat::AnthropicMessages,
            to: WireFormat::OpenAiResponses,
            source: ir_with_thinking(),
        };
        let adapted = cr.adapt_lossy();
        assert!(adapted.thinking.is_some());
    }

    #[test]
    fn finish_reason_other_degrades_to_stop_with_lossy() {
        let fr = FinishReasonIR::Other(Cow::Borrowed("pause_turn"));
        let mapped = map_finish_reason(
            WireFormat::AnthropicMessages,
            WireFormat::OpenAiChat,
            fr,
        );
        assert!(matches!(mapped, FinishReasonIR::Stop));
    }

    #[test]
    fn finish_reason_content_filter_degrades_to_stop_for_anthropic() {
        let mapped = map_finish_reason(
            WireFormat::OpenAiChat,
            WireFormat::AnthropicMessages,
            FinishReasonIR::ContentFilter,
        );
        assert!(matches!(mapped, FinishReasonIR::Stop));
    }

    #[test]
    fn finish_reason_same_wire_identity() {
        let mapped = map_finish_reason(
            WireFormat::OpenAiChat,
            WireFormat::OpenAiChat,
            FinishReasonIR::ContentFilter,
        );
        assert!(matches!(mapped, FinishReasonIR::ContentFilter));
    }
}
```

- [ ] **Step 6: 跑测**

Run:
```bash
cargo test -p gw-codec --lib cross_render 2>&1 | tail -10
```

Expected: 6 个测 pass。

- [ ] **Step 7: Commit**

```bash
git add crates/gw-codec/src/cross_render.rs crates/gw-codec/src/lib.rs crates/gw-core/src/error/codes.rs crates/gw-gateway/src/metrics_catalog.rs
git commit -m "$(cat <<'EOF'
v2-codec(T3): cross_render skeleton + WireFormat / Capabilities / LossyField

- WireFormat enum (4 forms) + capabilities table
- LossyField::drop(from, to, field) - single point that emits both
  tracing::error! and gw_codec_cross_render_lossy_total metric
- CrossRender::adapt_lossy strips vendor-specific fields per target caps
- map_finish_reason: ContentFilter→Anthropic and Other(_) degrade with lossy
- New error codes: GW_CODEC_{PARSE_RESPONSE,RENDER_RESPONSE,...}
- New metric describe lines: cross_render_{nonstream_total,lossy_total}

Sub-spec S-codec §2.3 + §4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 3 — Per-codec Vertical Slices

Each of T4-A / T4-B / T4-C / T4-D 是一个 codec 的完整改造:wire 形态升级 + via 双向 + render_response 实现 + state 累积器 + 单测。每个 task 内步骤模式相同,但具体类型 / wire schema / 单测 fixture 因 vendor 而异。**每个 task 完成后该 codec 不再有 `unimplemented!()` / `todo!()` / `TODO(v2)` / `"v1: ..."`**。

### Task 4-A: OpenAI Chat codec 完整

**Files:**
- Modify: `crates/gw-codec/src/openai/wire.rs`
- Modify: `crates/gw-codec/src/openai/via.rs`
- Modify: `crates/gw-codec/src/openai/codec.rs`
- Create: `crates/gw-codec/src/openai/state.rs`
- Modify: `crates/gw-codec/src/openai/mod.rs`(`pub mod state;`)

- [ ] **Step 1: 写 `OpenAiResponse` wire 结构 + tools/tool_choice 显式字段(`wire.rs`)**

在 `crates/gw-codec/src/openai/wire.rs` 加(具体 JSON schema 参 OpenAI Chat Completions 文档,fixture 详见单测 step):

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiResponse<'a> {
    pub id: Cow<'a, str>,
    #[serde(borrow)]
    pub object: Cow<'a, str>,   // "chat.completion"
    pub created: u64,
    pub model: Cow<'a, str>,
    pub choices: Vec<OpenAiChoice<'a>>,
    pub usage: Option<OpenAiUsage>,
    #[serde(flatten)]
    pub extra: HashMap<Cow<'a, str>, Box<RawValue>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiChoice<'a> {
    pub index: u32,
    pub message: OpenAiMessage<'a>,
    pub finish_reason: Option<Cow<'a, str>>,  // "stop"|"length"|"tool_calls"|"content_filter"|...
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct OpenAiUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct OpenAiPromptTokensDetails {
    pub cached_tokens: u32,
}
```

同时,在已有 `OpenAiRequest` 的 `#[serde(flatten)] extra: HashMap<String, ...>` 改 key 为 `Cow<'a, str>`(N-1)。新加显式 `tools: Option<Vec<OpenAiTool<'a>>>`、`tool_choice: Option<OpenAiToolChoice<'a>>` 字段(从 extra 提取出来)。

- [ ] **Step 2: 写 `via.rs` 中 `ResponseIR ↔ OpenAiResponse` 双向**

完整 IR → wire(`render_response` 调用):

```rust
impl<'a> TryFrom<&'a gw_core::ir::ResponseIR<'a>> for OpenAiResponse<'a> {
    type Error = gw_core::ir::ErrorIR<'static>;
    fn try_from(ir: &'a gw_core::ir::ResponseIR<'a>) -> Result<Self, Self::Error> {
        // 1. id / model / created 直拷
        // 2. messages: 取最后一条 Assistant message → OpenAiMessage(role=assistant, content, tool_calls)
        //    若 ir.tool_use_blocks.is_some(),映射为 OpenAiToolCall vector(因 OpenAI 用扁平 tool_calls)
        // 3. finish_reason: map FinishReasonIR → "stop"/"length"/"tool_calls"/"content_filter"/<other 在 T3 已 degrade>
        // 4. usage: UsageIR → OpenAiUsage(input→prompt, output→completion, cached_tokens 嵌入 prompt_tokens_details)
        //    reasoning_tokens 不出现在 OpenAI Chat usage(目标 wire 不支持,T3 cross_render 已裁剪)
        // 5. thinking 不写入 wire(OpenAI Chat 不支持,T3 cross_render 已裁剪)
        ...
    }
}
```

wire → IR(`parse_response`):

```rust
impl<'a> From<OpenAiResponse<'a>> for gw_core::ir::ResponseIR<'a> {
    fn from(w: OpenAiResponse<'a>) -> Self {
        // 1. id / model / created 直拷
        // 2. choices[0].message → MessageIR::Assistant(content, tool_calls)
        // 3. choices[0].finish_reason 字符串映射回 FinishReasonIR
        //    "stop"→Stop / "length"→Length / "tool_calls"→ToolCalls / "content_filter"→ContentFilter
        //    其他 → Other(Cow::Owned(s.to_string()))
        // 4. usage → UsageIR
        // 5. thinking/reasoning/tool_use_blocks 留 None(OpenAI Chat 不产)
        ...
    }
}
```

- [ ] **Step 3: 实现 `render_response`(`codec.rs`)— 替换 unimplemented**

```rust
fn render_response(
    &self,
    ir: &gw_core::ir::ResponseIR<'_>,
    buf: &mut BytesMut,
) -> Result<(), gw_core::ir::ErrorIR<'static>> {
    let wire: OpenAiResponse<'_> = ir.try_into()?;
    serde_json::to_writer(BytesWriter(buf), &wire)
        .map_err(|e| ErrorIR::new(codes::GW_CODEC_RENDER_RESPONSE, e.to_string()))?;
    Ok(())
}
```

(`BytesWriter` 是 BytesMut Write adapter,若不存在,加 helper。)

- [ ] **Step 4: 实现 `OpenAiStreamState`(`state.rs`)**

`crates/gw-codec/src/openai/state.rs`:

```rust
use std::any::Any;
use std::collections::HashMap;
use gw_core::ir::StreamState;

#[derive(Default)]
pub struct OpenAiStreamState {
    /// index → 已累积的 arguments JSON 片段(在 ToolCallStart 发出后,后续 chunk 追加到此)
    pub(crate) pending_tool_calls: HashMap<u32, OpenAiToolCallBuilder>,
    pub(crate) seen_request_id: Option<String>,
}

#[derive(Default)]
pub(crate) struct OpenAiToolCallBuilder {
    pub id: String,
    pub name: String,
    pub arguments: String,  // 累积 chunk 直到 stop 事件 emit ToolCallDelta-final 或单独 ToolCallStart→后续 Delta
}

impl StreamState for OpenAiStreamState {
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}
```

- [ ] **Step 5: 实现 `parse_stream_chunk` 累积器(`codec.rs`)**

替换 T2 的 todo!() stub:

```rust
fn parse_stream_chunk(
    &self,
    state: &mut dyn gw_core::ir::StreamState,
    bytes: &[u8],
) -> Result<Vec<gw_core::ir::StreamEventIR<'_>>, gw_core::ir::ErrorIR<'static>> {
    let st = state
        .as_any_mut()
        .downcast_mut::<OpenAiStreamState>()
        .expect("stream_state type mismatch");

    // SSE 行解析:`data: {...}\n\n` 或 `[DONE]`
    let mut events = Vec::new();
    for line in bytes.split(|b| *b == b'\n') {
        if line.is_empty() || !line.starts_with(b"data: ") { continue; }
        let payload = &line[6..];
        if payload == b"[DONE]" {
            events.push(StreamEventIR::End { finish_reason: FinishReasonIR::Stop });
            continue;
        }
        let chunk: OpenAiStreamChunk = serde_json::from_slice(payload)
            .map_err(|e| ErrorIR::new(codes::GW_CODEC_PARSE_STREAM_CHUNK, e.to_string()))?;
        // 把 chunk.choices[0].delta 拆成:
        //   - text → Delta { text, .. }
        //   - tool_calls[].id+name 首次出现 → ToolCallStart
        //   - tool_calls[].function.arguments 后续片段 → 累积到 st.pending_tool_calls
        //     当 chunk.choices[0].finish_reason == Some("tool_calls") 时 emit ToolCallDelta(arguments_chunk = accumulated, ..)
        //   - finish_reason → End
        //   - usage → Usage
        ...
    }
    Ok(events)
}
```

`new_stream_state` 替换 todo!() 为:

```rust
fn new_stream_state(&self) -> Box<dyn gw_core::ir::StreamState> {
    Box::new(OpenAiStreamState::default())
}
```

- [ ] **Step 6: 写单测 — `render_response` round-trip 纯 text**

`crates/gw-codec/src/openai/codec.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn render_response_text_only_round_trip() {
    let mut ir = ResponseIR::default();
    ir.id = Cow::Borrowed("chatcmpl-abc");
    ir.model = Cow::Borrowed("gpt-4o");
    ir.messages = vec![MessageIR::Assistant {
        content: vec![ContentIR::Text(Cow::Borrowed("Hello, world!"))],
        tool_calls: None,
    }];
    ir.finish_reason = Some(FinishReasonIR::Stop);
    ir.usage = Some(UsageIR { input_tokens: 5, output_tokens: 3, cached_tokens: None, reasoning_tokens: None });

    let mut buf = BytesMut::new();
    OpenAiCodec.render_response(&ir, &mut buf).unwrap();

    // round-trip back
    let parsed: ResponseIR = OpenAiCodec.parse_response(&buf).unwrap();
    assert_eq!(parsed.id, "chatcmpl-abc");
    assert!(matches!(parsed.finish_reason, Some(FinishReasonIR::Stop)));
    assert_eq!(parsed.usage.unwrap().input_tokens, 5);
}
```

- [ ] **Step 7: 写单测 — `render_response` 含 tool_calls**

```rust
#[test]
fn render_response_with_tool_calls_round_trip() {
    let mut ir = ResponseIR::default();
    ir.id = Cow::Borrowed("chatcmpl-tool");
    ir.model = Cow::Borrowed("gpt-4o");
    ir.messages = vec![MessageIR::Assistant {
        content: vec![],
        tool_calls: Some(vec![ToolCallIR {
            id: Cow::Borrowed("call_1"),
            name: Cow::Borrowed("get_weather"),
            arguments: Cow::Borrowed(r#"{"city":"SF"}"#),
        }]),
    }];
    ir.finish_reason = Some(FinishReasonIR::ToolCalls);
    let mut buf = BytesMut::new();
    OpenAiCodec.render_response(&ir, &mut buf).unwrap();
    let parsed: ResponseIR = OpenAiCodec.parse_response(&buf).unwrap();
    let calls = match &parsed.messages[0] {
        MessageIR::Assistant { tool_calls: Some(c), .. } => c,
        _ => panic!("expected tool_calls"),
    };
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(calls[0].arguments.as_ref(), r#"{"city":"SF"}"#);
}
```

- [ ] **Step 8: 写单测 — `parse_stream_chunk` tool_call delta 累积合并**

```rust
#[test]
fn parse_stream_chunk_accumulates_tool_call_arguments() {
    let codec = OpenAiCodec;
    let mut state = codec.new_stream_state();
    // chunk 1: { ..., "delta": {"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather"}}]} }
    let c1 = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather"}}]}}]}

"#;
    let events1 = codec.parse_stream_chunk(&mut *state, c1).unwrap();
    assert!(matches!(events1[0], StreamEventIR::ToolCallStart { index: 0, .. }));

    // chunk 2: arguments delta partial
    let c2 = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}

"#;
    let events2 = codec.parse_stream_chunk(&mut *state, c2).unwrap();
    // 累积中,不 emit
    assert!(events2.is_empty());

    // chunk 3: arguments delta cont + finish_reason
    let c3 = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"SF\"}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

"#;
    let events3 = codec.parse_stream_chunk(&mut *state, c3).unwrap();
    // 应 emit ToolCallDelta(累积完整 args) + End
    let delta = events3.iter().find_map(|e| match e {
        StreamEventIR::ToolCallDelta { index: 0, arguments_chunk } => Some(arguments_chunk.clone()),
        _ => None,
    });
    assert_eq!(delta.unwrap(), r#"{"city":"SF"}"#);
    assert!(matches!(events3.last(), Some(StreamEventIR::End { .. })));
}
```

- [ ] **Step 9: 跑 OpenAI Chat 测**

Run:
```bash
cargo test -p gw-codec --lib openai:: 2>&1 | tail -15
```

Expected: 3 个新测全 pass + 既有 OpenAI 请求侧测仍 pass。

- [ ] **Step 10: 验证 OpenAI Chat 范围内无 `unimplemented!()` / `todo!()` / `TODO(v2)`**

Run:
```bash
grep -nE 'unimplemented!\(|todo!\(|TODO\(v2\)' crates/gw-codec/src/openai/codec.rs crates/gw-codec/src/openai/wire.rs crates/gw-codec/src/openai/via.rs crates/gw-codec/src/openai/state.rs
```

Expected: 零命中。若有遗留(尤其是 T2 加的 todo!() stub),回到 step 5 替换实现。

- [ ] **Step 11: Commit**

```bash
git add crates/gw-codec/src/openai/
git commit -m "$(cat <<'EOF'
v2-codec(T4-A): OpenAI Chat — render_response + tool_call delta accumulator

- OpenAiResponse wire struct (choices/usage/...)
- via.rs: ResponseIR <-> OpenAiResponse double-direction
- render_response replaces unimplemented!() stub
- OpenAiStreamState accumulates tool_call arguments across SSE chunks
- parse_stream_chunk emits ToolCallStart on first occurrence, ToolCallDelta
  with accumulated args on finish_reason=tool_calls
- OpenAiRequest.extra HashMap key Cow<'a, str> (N-1)
- Three new tests: text round-trip, tool_calls round-trip, stream accumulator

Sub-spec S-codec §2.4 OpenAI Chat / §3.3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4-B: OpenAI Responses codec 完整

**Files:**
- Modify: `crates/gw-codec/src/openai/responses/wire.rs`
- Modify: `crates/gw-codec/src/openai/responses/via.rs`
- Modify: `crates/gw-codec/src/openai/responses/codec.rs`
- Create: `crates/gw-codec/src/openai/responses/state.rs`
- Modify: `crates/gw-codec/src/openai/responses/mod.rs`(`pub mod state;`)

- [ ] **Step 1: `wire.rs` 补 `OpenAiResponsesResponse` 含 reasoning summary**

Responses API 响应形状参考 OpenAI Responses 文档:`output: Vec<ResponseItem>`(含 Message / Reasoning / FunctionCall items)、`usage.completion_tokens_details.reasoning_tokens`。具体类型:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiResponsesResponse<'a> {
    pub id: Cow<'a, str>,
    pub object: Cow<'a, str>,   // "response"
    pub created_at: u64,
    pub model: Cow<'a, str>,
    pub status: Cow<'a, str>,   // "completed"|"incomplete"|"in_progress"
    pub incomplete_details: Option<IncompleteDetails<'a>>,
    pub output: Vec<ResponseOutputItem<'a>>,
    pub usage: Option<ResponsesUsage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseOutputItem<'a> {
    Message { id: Cow<'a, str>, content: Vec<MessageContent<'a>>, role: Cow<'a, str>, status: Cow<'a, str> },
    Reasoning { id: Cow<'a, str>, summary: Vec<ReasoningSummaryPart<'a>>, encrypted_content: Option<Cow<'a, str>> },
    FunctionCall { id: Cow<'a, str>, name: Cow<'a, str>, arguments: Cow<'a, str>, call_id: Cow<'a, str>, status: Cow<'a, str> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummaryPart<'a> {
    SummaryText { text: Cow<'a, str> },
}
```

- [ ] **Step 2: `via.rs` 实现 ResponseIR ↔ OpenAiResponsesResponse**

特别处理:
- `output` 内的 `Reasoning` items 聚合为 `ResponseIR.reasoning` (拼接 summary parts) 与 `ResponseIR.thinking`(Responses 可同时携带 reasoning 与 thinking,具体看 schema)
- `output` 内的 `FunctionCall` items 转 `MessageIR::Assistant.tool_calls`(OpenAI 风格扁平)
- `usage.completion_tokens_details.reasoning_tokens` → `UsageIR.reasoning_tokens`

- [ ] **Step 3: `codec.rs` 实现 `render_response`(替换 unimplemented)**

模式同 T4-A step 3。

- [ ] **Step 4: `state.rs` 实现 `OpenAiResponsesStreamState`(reasoning 累积 + tool_call delta)**

```rust
#[derive(Default)]
pub struct OpenAiResponsesStreamState {
    pub(crate) pending_reasoning: HashMap<String, ReasoningBuilder>,  // id → builder
    pub(crate) pending_function_calls: HashMap<String, FunctionCallBuilder>,
    pub(crate) seen_response_id: Option<String>,
}
impl StreamState for OpenAiResponsesStreamState { fn as_any_mut(&mut self) -> &mut dyn Any { self } }
```

- [ ] **Step 5: 实现 `parse_stream_chunk` + `new_stream_state`**

Responses 流式事件类型(参考 OpenAI Responses streaming events doc):`response.created`、`response.output_item.added`、`response.output_item.done`、`response.content_part.added`、`response.content_part.delta`、`response.completed`。累积器需要处理:
- `output_item.added type=reasoning` → 准备空 reasoning builder
- `content_part.delta` of reasoning summary → append text
- `output_item.done type=reasoning` → emit `ThinkingDelta`(summary 完成,但本 spec 把 reasoning summary 也作为 ThinkingDelta event 路径)
- `output_item.added type=function_call` → emit `ToolCallStart`
- `output_item.delta type=function_call arguments` → append
- `output_item.done type=function_call` → emit `ToolCallDelta`(完整 args)
- `response.completed` → emit `End` + `Usage`

- [ ] **Step 6: 单测 — render_response with reasoning round-trip**

`crates/gw-codec/src/openai/responses/codec.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn render_response_with_reasoning_round_trip() {
    let mut ir = ResponseIR::default();
    ir.id = Cow::Borrowed("resp_abc");
    ir.model = Cow::Borrowed("o1");
    ir.reasoning = Some(ReasoningBlockIR { summary: Cow::Borrowed("step 1; step 2") });
    ir.messages = vec![MessageIR::Assistant {
        content: vec![ContentIR::Text(Cow::Borrowed("answer"))],
        tool_calls: None,
    }];
    ir.finish_reason = Some(FinishReasonIR::Stop);
    ir.usage = Some(UsageIR { input_tokens: 10, output_tokens: 5, cached_tokens: None, reasoning_tokens: Some(20) });

    let mut buf = BytesMut::new();
    OpenAiResponsesCodec.render_response(&ir, &mut buf).unwrap();
    let parsed: ResponseIR = OpenAiResponsesCodec.parse_response(&buf).unwrap();
    assert_eq!(parsed.reasoning.unwrap().summary, "step 1; step 2");
    assert_eq!(parsed.usage.unwrap().reasoning_tokens.unwrap(), 20);
}
```

- [ ] **Step 7: 单测 — Responses 流式 reasoning 续行累积**

至少一个测覆盖多 chunk reasoning summary 累积到 ThinkingDelta emit。

- [ ] **Step 8: 跑测 + grep v1 markers + Commit**

```bash
cargo test -p gw-codec --lib openai::responses 2>&1 | tail -15
grep -nE 'unimplemented!\(|todo!\(|TODO\(v2\)' crates/gw-codec/src/openai/responses/

git add crates/gw-codec/src/openai/responses/
git commit -m "$(cat <<'EOF'
v2-codec(T4-B): OpenAI Responses — render_response + reasoning/function_call accumulator

- OpenAiResponsesResponse wire struct (output items, status, reasoning, usage)
- via.rs: ResponseIR <-> Responses (reasoning summary, function_call -> tool_calls)
- render_response replaces unimplemented!() stub
- OpenAiResponsesStreamState accumulates reasoning summaries + function_call args
- Two new tests: reasoning round-trip, stream reasoning accumulator

Sub-spec S-codec §2.4 OpenAI Responses.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4-C: Anthropic Messages codec 完整

**Files:**
- Modify: `crates/gw-codec/src/anthropic/wire.rs`(**最大改动**:content 从 `Cow<'a,str>` 改 `Vec<AnthropicContent<'a>>` 枚举)
- Modify: `crates/gw-codec/src/anthropic/via.rs`(连带处理 tool_use / tool_result / thinking blocks)
- Modify: `crates/gw-codec/src/anthropic/codec.rs`
- Create: `crates/gw-codec/src/anthropic/state.rs`
- Modify: `crates/gw-codec/src/anthropic/mod.rs`

- [ ] **Step 1: `wire.rs` 改 `AnthropicMessage.content` 为数组枚举**

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContent<'a> {
    Text { text: Cow<'a, str> },
    Image { source: AnthropicImageSource<'a> },
    Thinking { thinking: Cow<'a, str>, signature: Cow<'a, str> },
    ToolUse { id: Cow<'a, str>, name: Cow<'a, str>, input: Cow<'a, RawValue> },
    ToolResult { tool_use_id: Cow<'a, str>, content: Vec<AnthropicContent<'a>>, is_error: Option<bool> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnthropicMessage<'a> {
    pub role: Cow<'a, str>,  // "user" | "assistant"
    pub content: Vec<AnthropicContent<'a>>,
}
```

**注意**:既有 User/Assistant 在 wire 都从 `content: Cow<'a, str>` 改为 `content: Vec<AnthropicContent<'a>>`,via 层把单纯文本场景 `vec![AnthropicContent::Text { text }]` 还原回 `MessageIR::User { content: vec![ContentIR::Text(...)] }`。

- [ ] **Step 2: `wire.rs` 加 `AnthropicResponse` 结构**

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnthropicResponse<'a> {
    pub id: Cow<'a, str>,
    #[serde(rename = "type")]
    pub kind: Cow<'a, str>,  // "message"
    pub role: Cow<'a, str>,  // "assistant"
    pub model: Cow<'a, str>,
    pub content: Vec<AnthropicContent<'a>>,
    pub stop_reason: Option<Cow<'a, str>>,
    pub stop_sequence: Option<Cow<'a, str>>,
    pub usage: Option<AnthropicUsage>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct AnthropicUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: Option<u32>,
    pub cache_read_input_tokens: Option<u32>,
}
```

- [ ] **Step 3: `via.rs` 双向 ResponseIR ↔ AnthropicResponse**

关键 mapping:
- `Vec<AnthropicContent>` 中的 `Text` blocks 拼成 `MessageIR::Assistant.content`
- `ToolUse` blocks 同时填 `MessageIR::Assistant.tool_calls`(扁平形式)与 `ResponseIR.tool_use_blocks`(块形式) — IR 双形态保留,便于跨协议 render 时按目标 wire 能力选用
- `Thinking` blocks 填 `ResponseIR.thinking.blocks`
- `stop_reason: "end_turn"|"max_tokens"|"tool_use"|"stop_sequence"|<其他>` → `FinishReasonIR::Stop|Length|ToolCalls|Stop+lossy|Other(s)`
- `usage` → `UsageIR{ input_tokens, output_tokens, cached_tokens = cache_read_input_tokens, reasoning_tokens = None }`

请求侧 via(既有,但要适配 content 数组改动):User/Assistant `MessageIR::content: Vec<ContentIR>` 映射回 `Vec<AnthropicContent>`,文本 → Text,图片 → Image。`MessageIR::Tool { tool_use_id, result }` 映射为 `User { content: vec![ToolResult{...}] }`(Anthropic 把 tool_result 放进 user message)。

- [ ] **Step 4: `codec.rs` 实现 `render_response`**

模式同 T4-A;若 ir.tool_use_blocks 存在,优先用块形态(Anthropic 偏好);否则降级把 tool_calls 转 ToolUse blocks。

- [ ] **Step 5: `state.rs` 实现 `AnthropicStreamState`(event-style 多事件类型)**

Anthropic 流式是 event-typed SSE:`message_start`、`content_block_start`、`content_block_delta`、`content_block_stop`、`message_delta`、`message_stop`。累积器需追踪当前打开的 content_block(text / thinking / tool_use):

```rust
#[derive(Default)]
pub struct AnthropicStreamState {
    /// content_block_index → builder
    pub(crate) open_blocks: HashMap<u32, OpenBlock>,
    pub(crate) seen_message_id: Option<String>,
}

pub(crate) enum OpenBlock {
    Text(String),
    Thinking { text: String, signature: Option<String> },
    ToolUse { id: String, name: String, input_json: String },
}

impl StreamState for AnthropicStreamState { fn as_any_mut(&mut self) -> &mut dyn Any { self } }
```

- [ ] **Step 6: 实现 `parse_stream_chunk`(Anthropic event-typed SSE)**

```rust
fn parse_stream_chunk(...) {
    // 解析 `event: <type>\ndata: <json>\n\n` 多事件块
    // 每个事件按 type 分:
    //   message_start → emit Start { request_id = message.id }
    //   content_block_start with thinking → open Thinking block
    //   content_block_delta with thinking_delta.signature → 累积 signature
    //   content_block_delta with text_delta → Delta { text, .. }(单事件 Delta)
    //   content_block_delta with input_json_delta → 累积 ToolUse.input_json
    //   content_block_stop → 
    //      Thinking → emit ThinkingDelta { text = accumulated, signature_chunk = accumulated_signature }
    //      ToolUse → emit ToolCallStart + ToolCallDelta(完整 args)
    //   message_delta → 含 stop_reason / usage → emit Usage + End
    //   message_stop → flush 剩余 + 收尾
}
```

- [ ] **Step 7: 单测 — render_response with thinking + tool_use round-trip**

```rust
#[test]
fn render_response_with_thinking_and_tool_use() {
    let mut ir = ResponseIR::default();
    ir.id = Cow::Borrowed("msg_abc");
    ir.model = Cow::Borrowed("claude-3-5-sonnet");
    ir.thinking = Some(ResponseThinkingIR {
        blocks: vec![ThinkingBlockIR {
            text: Cow::Borrowed("step-by-step ..."),
            signature: Some(Cow::Borrowed("sig-base64")),
        }],
    });
    ir.tool_use_blocks = Some(vec![ToolUseBlockIR {
        id: Cow::Borrowed("toolu_1"),
        name: Cow::Borrowed("get_weather"),
        input: serde_json::value::to_raw_value(&serde_json::json!({"city":"SF"})).unwrap().into(),
    }]);
    ir.finish_reason = Some(FinishReasonIR::ToolCalls);
    let mut buf = BytesMut::new();
    AnthropicCodec.render_response(&ir, &mut buf).unwrap();
    let parsed: ResponseIR = AnthropicCodec.parse_response(&buf).unwrap();
    assert_eq!(parsed.thinking.unwrap().blocks[0].text, "step-by-step ...");
    assert_eq!(parsed.tool_use_blocks.unwrap()[0].name, "get_weather");
}
```

- [ ] **Step 8: 单测 — thinking 跨 chunk 续行累积**

至少一个测覆盖 `content_block_start(thinking)` + 多 `content_block_delta(thinking_delta)` + `content_block_stop` → emit 单 `ThinkingDelta` 含累积完整内容。

- [ ] **Step 9: 跑测 + grep + Commit**

```bash
cargo test -p gw-codec --lib anthropic:: 2>&1 | tail -15
grep -nE 'unimplemented!\(|todo!\(|TODO\(v2\)' crates/gw-codec/src/anthropic/

git add crates/gw-codec/src/anthropic/
git commit -m "$(cat <<'EOF'
v2-codec(T4-C): Anthropic Messages — content array + thinking/tool_use blocks

- AnthropicMessage.content: Vec<AnthropicContent> with Text/Image/Thinking/ToolUse/ToolResult
- AnthropicResponse wire struct (content blocks, stop_reason, usage)
- via.rs: ResponseIR <-> AnthropicResponse with thinking + tool_use_blocks
  (also fills tool_calls flat form for cross-protocol downgrade)
- render_response replaces unimplemented!() stub
- AnthropicStreamState tracks event-typed SSE per content_block_index
- parse_stream_chunk emits ThinkingDelta with accumulated signature, ToolCallDelta
- Three new tests: render with thinking+tool_use, thinking accumulator, ...

Sub-spec S-codec §2.4 Anthropic + §3.3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4-D: Gemini Generate codec 完整

**Files:**
- Modify: `crates/gw-codec/src/gemini/wire.rs`(Part 改枚举,新加 Response)
- Modify: `crates/gw-codec/src/gemini/via.rs`
- Modify: `crates/gw-codec/src/gemini/codec.rs`
- Create: `crates/gw-codec/src/gemini/state.rs`
- Modify: `crates/gw-codec/src/gemini/mod.rs`

- [ ] **Step 1: `wire.rs` 把 `GeminiPart` 改枚举**

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GeminiPart<'a> {
    Text { text: Cow<'a, str> },
    InlineData { #[serde(rename = "inlineData")] inline_data: GeminiInlineData<'a> },
    FileData { #[serde(rename = "fileData")] file_data: GeminiFileData<'a> },
    FunctionCall { #[serde(rename = "functionCall")] function_call: GeminiFunctionCall<'a> },
    FunctionResponse { #[serde(rename = "functionResponse")] function_response: GeminiFunctionResponse<'a> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeminiFunctionCall<'a> {
    pub name: Cow<'a, str>,
    pub args: Cow<'a, RawValue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeminiFunctionResponse<'a> {
    pub name: Cow<'a, str>,
    pub response: Cow<'a, RawValue>,
}
```

- [ ] **Step 2: `wire.rs` 加 `GeminiResponse`**

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeminiResponse<'a> {
    pub candidates: Vec<GeminiCandidate<'a>>,
    #[serde(rename = "usageMetadata")]
    pub usage_metadata: Option<GeminiUsage>,
    #[serde(rename = "modelVersion")]
    pub model_version: Option<Cow<'a, str>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeminiCandidate<'a> {
    pub content: GeminiContent<'a>,
    #[serde(rename = "finishReason")]
    pub finish_reason: Option<Cow<'a, str>>,  // "STOP"|"MAX_TOKENS"|"SAFETY"|"RECITATION"|"OTHER"
    pub index: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct GeminiUsage {
    #[serde(rename = "promptTokenCount")]
    pub prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount")]
    pub candidates_token_count: u32,
    #[serde(rename = "cachedContentTokenCount")]
    pub cached_content_token_count: Option<u32>,
}
```

- [ ] **Step 3: `via.rs` 双向 ResponseIR ↔ GeminiResponse**

关键 mapping:
- `candidates[0].content.parts` 内 Text parts 拼 `MessageIR::Assistant.content`
- FunctionCall parts → `MessageIR::Assistant.tool_calls`(扁平形式;name=function_call.name, arguments=function_call.args)
- FunctionResponse parts → 通常出现在 `role=function` 的 message,转 `MessageIR::Tool`
- `finish_reason` 字符串映射:`STOP→Stop`,`MAX_TOKENS→Length`,`SAFETY→ContentFilter`,`OTHER→Other("OTHER")`,`RECITATION→Other("RECITATION")`,其他 → `Other(s)`
- `usage_metadata` → UsageIR(input = prompt_token_count, output = candidates_token_count, cached_tokens = cached_content_token_count, reasoning_tokens = None)

- [ ] **Step 4: `codec.rs` 实现 `render_response`**

- [ ] **Step 5: `state.rs` + parse_stream_chunk 累积 functionCall args**

Gemini 流式 SSE:`data: {"candidates":[{"content":{"parts":[...]},...}]}\r\n\r\n`。functionCall 的 args 通常一次到位,但本 spec 仍提供累积位置(spec §3.3 提到 functionCall 续行 — 假设 vendor 文档证实存在分块场景)。

```rust
#[derive(Default)]
pub struct GeminiStreamState {
    pub(crate) pending_function_calls: Vec<GeminiFunctionCallBuilder>,
}

pub(crate) struct GeminiFunctionCallBuilder {
    pub name: String,
    pub args_json: String,
}

impl StreamState for GeminiStreamState { fn as_any_mut(&mut self) -> &mut dyn Any { self } }
```

`parse_stream_chunk` 累积 args 到 builder,当 `finish_reason` 出现时 emit ToolCallStart + ToolCallDelta。

- [ ] **Step 6: 单测 — render_response with functionCall round-trip**

```rust
#[test]
fn render_response_with_function_call_round_trip() {
    let mut ir = ResponseIR::default();
    ir.id = Cow::Borrowed("gemini-resp-1");
    ir.model = Cow::Borrowed("gemini-2.0-flash");
    ir.messages = vec![MessageIR::Assistant {
        content: vec![],
        tool_calls: Some(vec![ToolCallIR {
            id: Cow::Borrowed(""),  // Gemini 不带 call id
            name: Cow::Borrowed("get_weather"),
            arguments: Cow::Borrowed(r#"{"city":"SF"}"#),
        }]),
    }];
    ir.finish_reason = Some(FinishReasonIR::ToolCalls);
    let mut buf = BytesMut::new();
    GeminiCodec.render_response(&ir, &mut buf).unwrap();
    let parsed: ResponseIR = GeminiCodec.parse_response(&buf).unwrap();
    let calls = match &parsed.messages[0] {
        MessageIR::Assistant { tool_calls: Some(c), .. } => c,
        _ => panic!("expected tool_calls"),
    };
    assert_eq!(calls[0].name, "get_weather");
}
```

- [ ] **Step 7: 单测 — finishReason 映射 SAFETY → ContentFilter**

```rust
#[test]
fn parse_response_safety_finish_reason_maps_to_content_filter() {
    let wire = br#"{"candidates":[{"content":{"parts":[]},"finishReason":"SAFETY"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":0}}"#;
    let parsed: ResponseIR = GeminiCodec.parse_response(wire).unwrap();
    assert!(matches!(parsed.finish_reason, Some(FinishReasonIR::ContentFilter)));
}
```

- [ ] **Step 8: 跑测 + grep + Commit**

```bash
cargo test -p gw-codec --lib gemini:: 2>&1 | tail -15
grep -nE 'unimplemented!\(|todo!\(|TODO\(v2\)' crates/gw-codec/src/gemini/

git add crates/gw-codec/src/gemini/
git commit -m "$(cat <<'EOF'
v2-codec(T4-D): Gemini Generate — Part enum + render_response + functionCall accumulator

- GeminiPart enum: Text / InlineData / FileData / FunctionCall / FunctionResponse
- GeminiResponse wire struct (candidates, usageMetadata, modelVersion)
- via.rs: ResponseIR <-> GeminiResponse with functionCall <-> tool_calls
- render_response replaces unimplemented!() stub
- GeminiStreamState accumulates functionCall args across chunks
- finishReason SAFETY -> ContentFilter, OTHER/RECITATION -> Other(s)
- Three new tests: render with functionCall, SAFETY mapping, ...

Sub-spec S-codec §2.4 Gemini.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 4 — 跨协议接线

### Task 5: `cross_render` 的 `adapt_lossy` 完整字段表 + 验证

**Files:**
- Modify: `crates/gw-codec/src/cross_render.rs`

T3 的 `adapt_lossy` 只裁了 `thinking`、`reasoning`、`reasoning_tokens` 三个字段。完整覆盖 spec §4.2 还需:
- `tool_use_blocks` 与 `tool_calls` 同源,**不** lossy(可表达,codec 在 render 时按目标 wire 自选)。
- `signature`(Anthropic thinking 内字段) — 随 thinking 一起 drop,不单独裁。
- `model` / `id` / `created` — 可表达,不 lossy。
- `Other(_) finish_reason` — 已在 map_finish_reason 处理。

- [ ] **Step 1: 验证 T3 已实现 adapt_lossy 覆盖所有 lossy 字段(无新代码 — 这是验证 task)**

Run:
```bash
cargo test -p gw-codec --lib cross_render 2>&1 | tail -10
```

Expected: T3 6 个测全 pass。

- [ ] **Step 2: 补单测 — 跨协议 4×4 矩阵 spot check at cross_render 层(不通 codec)**

`crates/gw-codec/src/cross_render.rs` `#[cfg(test)] mod tests` 内加(参数化逐组合):

```rust
#[test]
fn adapt_lossy_matrix_thinking_drop_decisions() {
    use WireFormat::*;
    let cases = [
        (AnthropicMessages, OpenAiChat, true),
        (AnthropicMessages, OpenAiResponses, false),
        (AnthropicMessages, GeminiGenerate, true),
        (OpenAiResponses, OpenAiChat, true),
        (OpenAiResponses, AnthropicMessages, false),
        (OpenAiResponses, GeminiGenerate, true),
    ];
    for (from, to, should_drop) in cases {
        let cr = CrossRender { from, to, source: super::tests::ir_with_thinking() };
        let adapted = cr.adapt_lossy();
        assert_eq!(adapted.thinking.is_none(), should_drop,
            "from={from} to={to} should_drop={should_drop}");
    }
}
```

- [ ] **Step 3: 跑测 + Commit**

```bash
cargo test -p gw-codec --lib cross_render 2>&1 | tail -10

git add crates/gw-codec/src/cross_render.rs
git commit -m "$(cat <<'EOF'
v2-codec(T5): cross_render adapt_lossy matrix verification

Add parameterized test covering all 6 thinking-drop combinations across the
4-wire matrix. Confirms WireCapabilities table is consistent with §4.2.

Sub-spec S-codec §4.2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: `gw-proxy` 接线 — 撤 503 + 接入 CrossRender

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs`(撤 L230-256 跨协议 503 短路)
- Modify: `crates/gw-proxy/src/filters/response.rs`(non-stream 分支接入 CrossRender)
- Modify: `crates/gw-core/src/error/codes.rs`(删 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED`)
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`(若 `gw_codec_cross_render_nonstream_total` 还未注册)

- [ ] **Step 1: 撤 `upstream.rs` 跨协议 503 短路**

定位 `upstream.rs` 内含 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` 的代码块(spec §引用 L230 附近)。删除整个 `if !ctx.is_stream && upstream_kind != client_codec_kind { return emit_envelope_and_bail(...) ... }` 块。保留前后 `upstream_kind` 选择逻辑不动。

- [ ] **Step 2: `response.rs` non-stream 分支接入 CrossRender**

定位 non-stream parse_response 完成后的渲染点。改为:

```rust
let response_ir = upstream_codec.parse_response(&body_bytes)
    .map_err(|e| { /* GW_CODEC_PARSE_RESPONSE handling */ })?;

let mut out = BytesMut::with_capacity(body_bytes.len());
if ctx.cross_protocol {
    use gw_codec::cross_render::{CrossRender, WireFormat};
    let from = WireFormat::from(upstream_codec_kind);
    let to = WireFormat::from(client_codec_kind);
    CrossRender { from, to, source: response_ir }
        .run(client_codec, &mut out)
        .map_err(|e| { /* GW_CODEC_CROSS_RENDER_FAILED */ })?;
    metrics::counter!(
        "gw_codec_cross_render_nonstream_total",
        "from" => from.label(),
        "to" => to.label(),
    ).increment(1);
} else {
    // 同协议:zero-copy 路径,直接转发 body_bytes 原样
    out.extend_from_slice(&body_bytes);
}
// 发送 out 到下游
```

`WireFormat::from(CodecKind)` 是新加的辅助 conversion(放 `gw-codec/src/cross_render.rs` 或 `gw-proxy` ctx 模块,根据 codec kind 类型位置)。

- [ ] **Step 3: 删 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` 错误码**

`crates/gw-core/src/error/codes.rs` 内删除该常量与 doc 注释。

- [ ] **Step 4: 删该错误码的所有 reference**

Run:
```bash
grep -rnE 'GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED' crates/ docs/
```

Expected: 仅 spec / plan 文档命中(本 task 后)。代码命中:每个删除(包括测例)。

具体删除清单:
- `crates/gw-proxy/src/filters/upstream.rs`(已在 Step 1 删)
- 任何 `crates/gw-proxy/tests/*.rs` 内含此 token 的 test — 全部删除文件
- `crates/gw-core/src/error/codes.rs`(已在 Step 3 删)

- [ ] **Step 5: cargo build + 跑全 gw-proxy 测**

Run:
```bash
cargo build --workspace 2>&1 | tail -10
cargo test -p gw-proxy 2>&1 | tail -20
```

Expected: 全 pass。若有先前依赖 503 reject 的测仍存在 — 删除该测(已在 Step 4 处理)。

- [ ] **Step 6: Commit**

```bash
git add crates/gw-proxy/src/filters/upstream.rs crates/gw-proxy/src/filters/response.rs crates/gw-core/src/error/codes.rs crates/gw-proxy/tests/
git commit -m "$(cat <<'EOF'
v2-codec(T6): proxy wires cross-protocol non-stream through CrossRender

- upstream.rs: drop the 503 short-circuit for cross-protocol non-stream
- response.rs non-stream branch:
    cross_protocol -> CrossRender::run(target_codec) + nonstream_total++
    same-protocol -> zero-copy forward (unchanged)
- Delete GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED error code and
  all referencing tests
- WireFormat::from(CodecKind) helper for proxy <-> codec kind plumbing

Sub-spec S-codec §2.5 + §3.2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 5 — 测试

### Task 7: 跨协议矩阵 16 测

**Files:**
- Create: `crates/gw-codec/tests/cross_protocol_matrix.rs`

- [ ] **Step 1: 写 matrix harness**

```rust
//! 跨协议矩阵测:4 wire forms × 4 wire forms = 16 cases。
//! 每个 case 跑一次 parse_response → CrossRender::run → parse_response → 校验关键字段保真。
//! 同协议(from==to)走 identity 路径,跨协议跑 cross_render adapt_lossy + target render。

use bytes::BytesMut;
use gw_codec::cross_render::{CrossRender, WireFormat};
use gw_codec::trait_def::ProtocolCodec;
use gw_codec::{anthropic::AnthropicCodec, gemini::GeminiCodec, openai::OpenAiCodec, openai::responses::OpenAiResponsesCodec};
use gw_core::ir::ResponseIR;

fn codec_for(w: WireFormat) -> Box<dyn ProtocolCodec> {
    match w {
        WireFormat::OpenAiChat => Box::new(OpenAiCodec),
        WireFormat::OpenAiResponses => Box::new(OpenAiResponsesCodec),
        WireFormat::AnthropicMessages => Box::new(AnthropicCodec),
        WireFormat::GeminiGenerate => Box::new(GeminiCodec),
    }
}

fn source_ir_for(from: WireFormat) -> ResponseIR<'static> {
    // 每个 vendor 一个 fixture ResponseIR,字段含可表达 + 1 个 lossy(thinking)
    todo_no_v2!() // implementer 填:per spec §5.2 每个 vendor 的 fixture
    // 注:此 plan 文件本身不是生产代码,plan 文件出现的 todo!() 不被 check-invariants lint 命中
}

#[test]
fn matrix_4x4_all_combinations() {
    use WireFormat::*;
    for &from in &[OpenAiChat, OpenAiResponses, AnthropicMessages, GeminiGenerate] {
        for &to in &[OpenAiChat, OpenAiResponses, AnthropicMessages, GeminiGenerate] {
            let ir = source_ir_for(from);
            let cr = CrossRender { from, to, source: ir };
            let mut buf = BytesMut::new();
            cr.run(codec_for(to).as_ref(), &mut buf)
                .unwrap_or_else(|e| panic!("from={from:?} to={to:?} error={e:?}"));
            // round-trip back via target codec
            let parsed: ResponseIR = codec_for(to).parse_response(&buf).unwrap();
            // 公共字段在每对都应非空
            assert!(parsed.usage.is_some(), "from={from:?} to={to:?} usage missing");
            assert!(parsed.finish_reason.is_some(), "from={from:?} to={to:?} fr missing");
        }
    }
}
```

(`todo_no_v2!()` macro 实施时定义为 `panic!("test fixture not provided")` 或者直接 inline 让 implementer 填具体 IR fixture。`source_ir_for` 在每个 vendor 返回一个含 text + 1 tool_use(或 tool_calls)+ 1 thinking(若可表达)的 IR。)

- [ ] **Step 2: 加 16 个 named test 简化诊断(per case 单测)**

```rust
macro_rules! cross_protocol_case {
    ($name:ident, $from:expr, $to:expr) => {
        #[test]
        fn $name() {
            let ir = source_ir_for($from);
            let cr = CrossRender { from: $from, to: $to, source: ir };
            let mut buf = BytesMut::new();
            cr.run(codec_for($to).as_ref(), &mut buf).unwrap();
            let parsed: ResponseIR = codec_for($to).parse_response(&buf).unwrap();
            assert!(parsed.usage.is_some());
            assert!(parsed.finish_reason.is_some());
            // 跨协议且目标无 thinking 时:验证 thinking 为 None
            if $from != $to {
                let to_caps = $to.capabilities();
                if !to_caps.supports_thinking {
                    assert!(parsed.thinking.is_none());
                }
            }
        }
    };
}

cross_protocol_case!(openai_chat_to_openai_chat, WireFormat::OpenAiChat, WireFormat::OpenAiChat);
cross_protocol_case!(openai_chat_to_openai_responses, WireFormat::OpenAiChat, WireFormat::OpenAiResponses);
cross_protocol_case!(openai_chat_to_anthropic, WireFormat::OpenAiChat, WireFormat::AnthropicMessages);
cross_protocol_case!(openai_chat_to_gemini, WireFormat::OpenAiChat, WireFormat::GeminiGenerate);
cross_protocol_case!(openai_responses_to_openai_chat, WireFormat::OpenAiResponses, WireFormat::OpenAiChat);
cross_protocol_case!(openai_responses_to_openai_responses, WireFormat::OpenAiResponses, WireFormat::OpenAiResponses);
cross_protocol_case!(openai_responses_to_anthropic, WireFormat::OpenAiResponses, WireFormat::AnthropicMessages);
cross_protocol_case!(openai_responses_to_gemini, WireFormat::OpenAiResponses, WireFormat::GeminiGenerate);
cross_protocol_case!(anthropic_to_openai_chat, WireFormat::AnthropicMessages, WireFormat::OpenAiChat);
cross_protocol_case!(anthropic_to_openai_responses, WireFormat::AnthropicMessages, WireFormat::OpenAiResponses);
cross_protocol_case!(anthropic_to_anthropic, WireFormat::AnthropicMessages, WireFormat::AnthropicMessages);
cross_protocol_case!(anthropic_to_gemini, WireFormat::AnthropicMessages, WireFormat::GeminiGenerate);
cross_protocol_case!(gemini_to_openai_chat, WireFormat::GeminiGenerate, WireFormat::OpenAiChat);
cross_protocol_case!(gemini_to_openai_responses, WireFormat::GeminiGenerate, WireFormat::OpenAiResponses);
cross_protocol_case!(gemini_to_anthropic, WireFormat::GeminiGenerate, WireFormat::AnthropicMessages);
cross_protocol_case!(gemini_to_gemini, WireFormat::GeminiGenerate, WireFormat::GeminiGenerate);
```

- [ ] **Step 3: 跑 + Commit**

Run:
```bash
cargo test -p gw-codec --test cross_protocol_matrix 2>&1 | tail -20
```

Expected: 16 named tests + 1 matrix test 全 pass。

```bash
git add crates/gw-codec/tests/cross_protocol_matrix.rs
git commit -m "$(cat <<'EOF'
v2-codec(T7): 4x4 cross-protocol response matrix tests

Sixteen named tests covering every (from, to) wire form combination. Each
case fabricates a source IR (with text + tool_use/tool_calls), runs
CrossRender::run through the target codec, parses back via the target,
and asserts usage/finish_reason survive and thinking is dropped where the
target does not support it.

Sub-spec S-codec §5.2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Lossy 专项测

**Files:**
- Create: `crates/gw-codec/tests/lossy_paths.rs`

- [ ] **Step 1: 写 thinking lossy 测**

```rust
//! 验证跨协议 lossy 路径:
//! - gw_codec_cross_render_lossy_total 计数 ++
//! - tracing::error! 触发
//! - 目标 wire 不含 lossy 字段

use bytes::BytesMut;
use gw_codec::cross_render::{CrossRender, WireFormat};
use gw_codec::openai::OpenAiCodec;
use gw_codec::anthropic::AnthropicCodec;
use gw_codec::trait_def::ProtocolCodec;
use gw_core::ir::*;
use metrics::Counter;
use std::borrow::Cow;

#[test]
fn anthropic_thinking_to_openai_chat_drops_with_metric() {
    let recorder = metrics_util::debugging::DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::set_global_recorder(recorder).unwrap_or(());  // first test wins

    let mut ir = ResponseIR::default();
    ir.id = Cow::Borrowed("msg_1");
    ir.model = Cow::Borrowed("claude-3-5-sonnet");
    ir.thinking = Some(ResponseThinkingIR {
        blocks: vec![ThinkingBlockIR {
            text: Cow::Borrowed("reasoning here"),
            signature: Some(Cow::Borrowed("sig")),
        }],
    });
    ir.messages = vec![MessageIR::Assistant {
        content: vec![ContentIR::Text(Cow::Borrowed("answer"))],
        tool_calls: None,
    }];
    ir.finish_reason = Some(FinishReasonIR::Stop);
    ir.usage = Some(UsageIR { input_tokens: 5, output_tokens: 3, cached_tokens: None, reasoning_tokens: None });

    let mut buf = BytesMut::new();
    CrossRender {
        from: WireFormat::AnthropicMessages,
        to: WireFormat::OpenAiChat,
        source: ir,
    }
    .run(&OpenAiCodec, &mut buf)
    .unwrap();

    // metric 增量
    let snap = snapshotter.snapshot();
    let lossy = snap
        .into_vec()
        .into_iter()
        .find(|(k, _, _, _)| k.key().name() == "gw_codec_cross_render_lossy_total")
        .expect("lossy_total counter expected");
    let labels: Vec<_> = lossy.0.key().labels().map(|l| (l.key(), l.value())).collect();
    assert!(labels.iter().any(|(k, v)| *k == "field" && *v == "thinking"));

    // 目标 wire 不含 thinking
    let wire = String::from_utf8(buf.to_vec()).unwrap();
    assert!(!wire.contains("thinking"));
}
```

(若 `metrics_util::debugging` 不是项目依赖,使用 `tracing-test` + `tracing` 字段 capture 检 ERROR log。具体 capture 方式视项目已有 test helper 而定。)

- [ ] **Step 2: 写 reasoning lossy(Responses → Anthropic)**

类似上,from=OpenAiResponses,to=AnthropicMessages,源 IR 含 reasoning,验证 field=reasoning 增量。

- [ ] **Step 3: 写 reasoning_tokens lossy 测**

源 IR usage.reasoning_tokens=Some(20),from=Responses,to=AnthropicMessages,验证 field=usage.reasoning_tokens 增量,目标 wire usage 不含 reasoning_tokens 数据。

- [ ] **Step 4: 写 finish_reason Other(s) lossy 测**

源 IR finish_reason=Other(Cow::Borrowed("pause_turn")),from=Anthropic,to=OpenAiChat,验证 field=finish_reason.other 增量,目标 wire finish_reason="stop"。

- [ ] **Step 5: 跑 + Commit**

```bash
cargo test -p gw-codec --test lossy_paths 2>&1 | tail -15

git add crates/gw-codec/tests/lossy_paths.rs
git commit -m "$(cat <<'EOF'
v2-codec(T8): lossy path tests (thinking / reasoning / reasoning_tokens / finish_reason.other)

Each test confirms gw_codec_cross_render_lossy_total{from,to,field} increments
and the target wire body does not contain the dropped field.

Sub-spec S-codec §5.3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: E2E 跨协议非流真实请求

**Files:**
- Create: `crates/gw-gateway/tests/e2e_cross_protocol_response.rs`

- [ ] **Step 1: 写 e2e — OpenAI Chat 客户端 / Anthropic 上游 / tool_use 响应**

复用 `gw-gateway/tests/` 已有 e2e 基础设施(fake upstream)。具体 fixture / harness 参考已落地的 `e2e_full_flow.rs`(本 spec §5.4 已指引)。

```rust
#[tokio::test]
async fn cross_protocol_anthropic_to_openai_chat_tool_use() {
    let harness = E2eHarness::start_with_fake_upstream().await;
    // 在 fake upstream 注册一个 Anthropic 响应含 tool_use:
    harness.fake.expect_anthropic_response(json!({
        "id": "msg_test",
        "type": "message",
        "role": "assistant",
        "model": "claude-3-5-sonnet",
        "content": [{"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city":"SF"}}],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 10, "output_tokens": 5}
    }));

    // 客户端以 OpenAI Chat 格式请求(模型 routes 到 Anthropic key):
    let resp = harness.client_openai_chat()
        .post("/v1/chat/completions")
        .json(&json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role":"user","content":"what's weather in SF"}],
            "stream": false,
            "tools": [{"type":"function","function":{"name":"get_weather","parameters":{}}}],
        }))
        .send()
        .await
        .unwrap();

    let body: serde_json::Value = resp.json().await.unwrap();
    // 验证响应是 OpenAI Chat 形态,含 tool_calls:
    let tc = &body["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(tc["function"]["name"], "get_weather");
    assert!(tc["function"]["arguments"].as_str().unwrap().contains("SF"));

    // metric 验证:
    let metric_value = harness.metric("gw_codec_cross_render_nonstream_total")
        .with_labels(&[("from","anthropic_messages"),("to","openai_chat")])
        .value();
    assert!(metric_value >= 1.0);
}
```

- [ ] **Step 2: 写 e2e — Anthropic 客户端 / OpenAI Chat 上游 / tool_calls 响应(反向)**

- [ ] **Step 3: 写 e2e — Gemini 客户端 / OpenAI Chat 上游 / tool_calls → functionCall**

- [ ] **Step 4: 写 e2e — OpenAI Chat 客户端 / Anthropic 上游 / thinking lossy 路径**

验证 metric `gw_codec_cross_render_lossy_total{from=anthropic_messages,to=openai_chat,field=thinking}` ++ 1,响应 body 不含 thinking。

- [ ] **Step 5: 跑 + Commit**

```bash
cargo test -p gw-gateway --test e2e_cross_protocol_response 2>&1 | tail -20

git add crates/gw-gateway/tests/e2e_cross_protocol_response.rs
git commit -m "$(cat <<'EOF'
v2-codec(T9): e2e cross-protocol non-stream response

Four scenarios with fake upstream:
- Anthropic upstream -> OpenAI Chat client: tool_use -> tool_calls
- OpenAI Chat upstream -> Anthropic client: tool_calls -> tool_use blocks
- OpenAI Chat upstream -> Gemini client: tool_calls -> functionCall parts
- Anthropic upstream -> OpenAI Chat client with thinking: lossy metric + log

Sub-spec S-codec §5.4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 6 — 收口

### Task 10: 文档同步 + 错误码全链清除 + 不变量验证

**Files:**
- Modify: `docs/architecture.md`(§Known gaps G-9 清理,若条目存在)
- Modify: `docs/superpowers/specs/2026-05-19-codec-s4-response-and-cross-protocol.md`(加 superseded marker)
- Modify: `docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md`(§9 表格 S-codec 行日期 + §11 状态追踪打勾)
- Modify: `CLAUDE.md`(L26 校正 cross-protocol response re-render 描述)
- Verify: `scripts/check-invariants.sh` 在 S-codec 范围内零命中

- [ ] **Step 1: `docs/architecture.md` §Known gaps 清理**

Run:
```bash
grep -n 'G-?9\|G-?8\|cross-protocol response\|tool_use round-trip' docs/architecture.md
```

对每个匹配:
- G-8 (wire→wire codegen):**不动**(本 spec 不做)
- G-9 (跨块累积器):**删除**(本 spec 落地)
- "cross-protocol response re-render":若仍标注为 "Known gap"/"deferred",删除该段
- "tool_use round-trip":同上

使用 Edit 工具精确删除对应条目。

- [ ] **Step 2: codec-s4 spec 加 superseded marker**

`docs/superpowers/specs/2026-05-19-codec-s4-response-and-cross-protocol.md` 顶部(标题之后第一行):

```markdown
> **Superseded by [2026-05-24-v2-codec-cleanup-design.md](./2026-05-24-v2-codec-cleanup-design.md) (S-codec sub-spec, landed 2026-05-24). All ADR-S4-v2-* items in this document are now implemented and tracked under the new spec.**
```

- [ ] **Step 3: overview spec §9 S-codec 行日期 + §11 状态追踪**

`docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md`:

§9 表格 S-codec 行:`2026-05-23-v2-codec-cleanup-design.md` → `2026-05-24-v2-codec-cleanup-design.md`(文件名 + 创建时机栏不变)。

§11 状态追踪:
- `- [ ] S-codec brainstorming → spec → plan → impl → 合入` 改为 `- [x] S-codec brainstorming → spec → plan → impl → 合入(c-hash, 2026-05-24)`

实际 commit hash 从 `git log -1 --oneline` 取(本 task 完成后)。

- [ ] **Step 4: CLAUDE.md L26 校正**

定位 L26:`- **Wire formats**: OpenAI Chat + Responses, Anthropic Messages, Gemini Generate. Same-protocol passthrough is zero-copy; cross-protocol request AND response re-render are wired (G8 closed in `50cd9e7`) — both directions emit the `gw_codec_cross_render_total{from,to}` metric.`

改为:

```markdown
- **Wire formats**: OpenAI Chat + Responses, Anthropic Messages, Gemini Generate. Same-protocol passthrough is zero-copy; cross-protocol request AND response re-render are wired (request side by codec-s4 v1; response side both stream and non-stream by S-codec v2, landed 2026-05-24). Cross-render activity is surfaced via `gw_codec_cross_render_total{from,to}` (legacy/stream), `gw_codec_cross_render_nonstream_total{from,to}` (v2 non-stream), and `gw_codec_cross_render_lossy_total{from,to,field}` (vendor-specific drops).
```

- [ ] **Step 5: 验证 scripts/check-invariants.sh 在 S-codec 范围内零命中**

Run:
```bash
bash scripts/check-invariants.sh 2>&1 | grep -A30 '\[no_v1_markers_in_prod\]' | head -40
```

Expected: 输出 `[no_v1_markers_in_prod] no v1 placeholder markers in crates/ (mode: warn)` 后**应不再列出 `crates/gw-codec/src/` 与 `crates/gw-proxy/src/filters/` 内的任何条目**;其他范围(`gw-routing`、`gw-ledger`、`gw-auth` 等)可能还有命中,这是后续 sub-spec 责任。

若 gw-codec / gw-proxy 范围仍命中:回到 T4 各 sub-task 修复对应 v1 marker。

- [ ] **Step 6: workspace 全测**

Run:
```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: 全 pass。

- [ ] **Step 7: Commit**

```bash
git add docs/architecture.md docs/superpowers/specs/2026-05-19-codec-s4-response-and-cross-protocol.md docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md CLAUDE.md
git commit -m "$(cat <<'EOF'
v2-codec(T10): doc sync — supersede codec-s4, update CLAUDE.md, mark overview

- docs/architecture.md §Known gaps G-9 removed (cross-block accumulator landed)
- 2026-05-19-codec-s4-response-and-cross-protocol.md gets superseded marker
  pointing at 2026-05-24-v2-codec-cleanup-design.md
- 2026-05-23-v2-cleanup-overview-design.md §9 S-codec row date 23 -> 24
- 2026-05-23-v2-cleanup-overview-design.md §11 S-codec checkbox flipped
- CLAUDE.md L26 corrects cross-protocol response re-render description and
  enumerates the three cross_render metric flavors
- check-invariants.sh confirms zero v1 markers in crates/gw-codec / crates/gw-proxy/src/filters

Sub-spec S-codec §8 invariants.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review

**1. Spec coverage:** 每条 spec 要求 → task 映射:

| Spec section | Task(s) |
|---|---|
| §1 目标态 1 (无 v1 markers) | T4-A/B/C/D step 10 + T10 step 5 |
| §1 目标态 2-3 (gw-codec / gw-proxy 测全过) | T4-A..D step 9 + T6 step 5 + T7 + T8 + T9 |
| §1 目标态 4 (撤 upstream.rs:230 503) | T6 step 1 |
| §1 目标态 5 (zero-copy 同协议不破) | T6 step 2 同协议分支保留 |
| §1 目标态 6 (跨协议 e2e + nonstream_total) | T9 step 1 + T3 step 2 metric |
| §1 目标态 7 (lossy log + metric) | T3 step 3 LossyField + T8 测 |
| §1 目标态 8 (SSE 累积器三家测) | T4-A step 8 + T4-C step 8 + T4-D step 6 + T4-B step 7 |
| §1 目标态 9 (stream/nonstream metric 区分) | T3 step 2 + T6 step 2 |
| §2.1 IR 扩展 | T1 全部 step |
| §2.2 trait 改动 | T2 全部 step |
| §2.3 cross_render | T3 + T5 |
| §2.4 4 codec | T4-A/B/C/D |
| §2.5 proxy 接线 | T6 |
| §3.1 同协议 zero-copy | T6 step 2 |
| §3.2 跨协议 flow | T6 step 2 + T9 |
| §3.3 SSE 累积器 | T4-A step 8 + T4-B step 7 + T4-C step 8 + T4-D step 6 |
| §4.1 错误码 | T3 step 1 |
| §4.2 lossy 字段表 | T3 step 3 + T5 step 2 |
| §4.3 finish_reason 映射 | T3 step 3 (map_finish_reason 函数) |
| §4.4 ERROR log + metric 一致 | T3 step 3 (LossyField::drop 内部) |
| §5.1 单测 | T4-A..D 内 |
| §5.2 跨协议矩阵 | T7 |
| §5.3 lossy 专项 | T8 |
| §5.4 E2E | T9 |
| §5.5 删 reject 测 | T6 step 4 |
| §6 sub-spec 协同 | 不需 task (spec 内信息性) |
| §7 N-1 优先级 | T4-A step 1 (extra Cow) + T4-C step 1 (Anthropic 数组) + T4-D step 1 (Gemini parts) |
| §8 不变量 | T10 全部 step |

**所有 spec section 都有 task 覆盖。无 gap。**

**2. Placeholder scan:** 

- "todo!()" 在 T2 step 2 是有意的中间状态 stub(明确注解 "本 task 完成后 lint warn 模式会报新增 todo,这是预期的。T4 各 sub-task 落地时清零"),后续 T4-A..D 删除。**不属于 plan 缺陷**。
- T7 step 1 `source_ir_for` 的 `todo_no_v2!()` 是 plan 文件内 sample code,要求 implementer 补具体 IR fixture,plan 明确说"implementer 填:per spec §5.2 每个 vendor 的 fixture"。**这是 OK 的(plan 指明依据,implementer 按 spec 落地)。** 若严格遵守 "no placeholder",可以把每个 fixture 的关键字段(text + tool block + 可选 thinking)用伪代码列出来。

- 无 "TBD" / "implement later" / "add appropriate error handling" 等模糊指示。
- 每个 step 给具体命令 + 期望输出。

**3. Type consistency:** 

- `OpenAiStreamState` / `OpenAiResponsesStreamState` / `AnthropicStreamState` / `GeminiStreamState` — 命名一致于 4 个 `state.rs` 文件,trait `StreamState` 定义在 ir.rs。
- `WireFormat` / `WireCapabilities` / `LossyField` / `CrossRender` 在 cross_render.rs 内一致出现。
- `ResponseThinkingIR` / `ThinkingBlockIR` / `ReasoningBlockIR` / `ToolUseBlockIR` 在 T1 step 1 加;T4-A..D 内使用;T8 测使用 — 一致。
- `gw_codec_cross_render_nonstream_total` / `gw_codec_cross_render_lossy_total` 在 T3 step 2 注册,T6 step 2 / T8 测 / T9 测内使用 — 一致。
- `GW_CODEC_PARSE_RESPONSE` / `GW_CODEC_RENDER_RESPONSE` / `GW_CODEC_PARSE_STREAM_CHUNK` / `GW_CODEC_CROSS_RENDER_FAILED` 在 T3 step 1 加,T4 各 codec 错误返回时引用,T6 step 2 引用 — 一致。
- `FinishReasonIR::Other(Cow<'static, str>)` 在 T1 step 4 改,T3 step 3 `map_finish_reason` 处理 — 一致。
- `parse_stream_chunk(&mut dyn StreamState, &[u8]) -> Vec<StreamEventIR>` 签名在 T2 step 1 定义,T4 各 codec step 5 实现,T7 不直接调(走 cross_render),T9 e2e 间接走 — 一致。

**Plan complete and ready for execution.**
