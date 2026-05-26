# Codec Cross-Protocol Gap 全量修复计划

## TL;DR

> **Quick Summary**: 修复 gw-codec 中 30 项跨协议转换 gap（P0×4, P1×12, P2×14），覆盖 OpenAI Chat/Responses、Anthropic Messages、Gemini Generate 四个 wire format，通过扩展 IR 类型 + wire struct + parse/render + adapt_lossy + WireCapabilities 实现完整协议覆盖。
> 
> **Deliverables**:
> - 扩展后的 `RequestIR` / `ResponseIR` / `StreamEventIR`（新增 ~15 个字段/变体）
> - 4 个 codec 的 wire struct 扩展（OpenAI Chat/Responses, Anthropic, Gemini）
> - `CrossRender::adapt_lossy` 全矩阵覆盖 + `WireCapabilities` 扩展
> - 每个 gap 的 round-trip 测试
> - `docs/architecture.md` + `docs/go-contracts/` 同步更新
> 
> **Estimated Effort**: XL
> **Parallel Execution**: YES - 5 waves
> **Critical Path**: Task 1 (IR) → Task 2 (WireCapabilities) → Task 9 (Gemini thinking) → Task 16 (adapt_lossy matrix) → Task 23 (integration test) → Task 25 (soak smoke) → F1-F4 (verification)

---

## Context

### Original Request
基于 2026-05 最新官方 API 文档对 gw-codec 进行 gap 分析后，修复全部 30 项跨协议转换缺陷。

### Interview Summary
**Key Discussions**:
- **修复范围**: 全部 30 项一次修完（P0+P1+P2），单 plan
- **建模策略**: 完整 Rust struct 建模，不使用 `Value` 透传（服务端工具 block 内部 payload 允许 `serde_json::Map` 作为 opaque inner）
- **Audio**: 纳入范围，新增 `ContentPart::Audio` + `AudioConfig` IR 概念
- **测试策略**: Tests-after（先实现再补测试）

**Research Findings**:
- IR 已有 `ContentPart::Audio{source: MediaSource}` 和 `ContentPart::File{source: MediaSource}` — Audio 和 File 的 IR 部分已就位
- OpenAI Chat wire `User.content` 仅 `String`，缺多模态 content parts
- Gemini `GenerationConfig` 仅 2/16 字段
- Anthropic wire 缺 8 种服务端工具 block 类型
- `cross_render.rs` adapt_lossy 仅处理 thinking/reasoning/reasoning_tokens/finish_reason

### Metis Review
**Identified Gaps** (addressed in plan):
- Refusal 建模: `FinishReasonIR::Refusal` + OpenAI wire `refusal` 字段
- Audio modality: IR ContentPart 已有 Audio，需在 OpenAI/Gemini wire 补齐
- 服务端工具 block: typed wrapper + `extra: serde_json::Map` for opaque inner
- reasoning_effort 统一: `ReasoningConfig{effort, budget_tokens}` IR 字段
- developer role: 映射到 Anthropic system / Gemini systemInstruction
- 每个新 IR 类型须在所有 4 codec 有 parse+render+adapt_lossy 入口

---

## Work Objectives

### Core Objective
扩展 gw-codec 的 IR + wire + parse/render + cross-render 层，消除 30 项跨协议转换 gap，使网关能忠实传递最新 OpenAI / Anthropic / Gemini API 的全部字段和语义。

### Concrete Deliverables
- `crates/gw-core/src/ir.rs` — 扩展 RequestIR, ResponseIR, StreamEventIR, FinishReasonIR, UsageIR, MessageIR, WireCapabilities 等
- `crates/gw-codec/src/openai/wire.rs` — OpenAI Chat wire struct 扩展
- `crates/gw-codec/src/openai/responses/wire_request.rs` + `wire_response.rs` + `wire_stream_chunk.rs` — Responses wire 扩展
- `crates/gw-codec/src/anthropic/wire.rs` — Anthropic wire struct 扩展
- `crates/gw-codec/src/gemini/wire.rs` — Gemini wire struct 扩展
- `crates/gw-codec/src/cross_render.rs` — adapt_lossy + WireCapabilities 全覆盖
- 4 个 codec 的 `via.rs` / `codec.rs` — parse/render 函数更新
- `docs/architecture.md`, `docs/go-contracts/` — 同步更新

### Definition of Done
- [ ] `cargo test --workspace` 全通过
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 无警告
- [ ] `bash scripts/check-invariants.sh` 退出 0
- [ ] `make e2e-soak-smoke` 退出 0
- [ ] 无 `todo!()` / `unimplemented!()` / `TODO(v2)` 标记

### Must Have
- 每个新 IR 字段在所有 4 codec 都有 parse + render + adapt_lossy 入口（即使是 stub + lossy counter）
- 每个新 lossy 映射必须 increment `gw_codec_cross_render_lossy_total{from,to,field}`
- OpenAI Chat User 多模态 content parts 完整支持（text, image_url, input_audio, file）
- Gemini GenerationConfig 扩展到至少 top_p, top_k, stop_sequences, presence_penalty, frequency_penalty, seed
- Anthropic 8 种服务端工具 block 类型完整建模
- OpenAI Chat `developer` 角色支持
- `FinishReasonIR::Refusal` 变体
- docs/architecture.md 更新（新 IR 类型/trait）
- docs/go-contracts/ 更新（如 usage:detailed payload 形状变化）

### Must NOT Have (Guardrails)
- 不得引入 `todo!()` / `unimplemented!()` / `TODO(v2)` / "for v1" 标记（lint 为 error 模式）
- 不得修改 same-protocol passthrough 行为（必须保持 zero-copy）
- 不得触碰 Pingora filter chain / routing / auth / ledger 代码路径
- 不得触碰计费/成本计算（`no_cost_micro`, `no_billing_crate` lints）
- 不得修改 CLI codec（bypass IR，不受影响）
- 不得引入 `serde_json::Value` 透传字段用于**响应 content block 建模**（以下例外允许 `Box<RawValue>` / `serde_json::Map` opaque 透传：(a) 服务端工具 block 内部 payload，(b) vendor-exclusive request 配置项如 `web_search_options`/`prediction`/`response_schema`，(c) vendor-exclusive response metadata 如 `grounding_metadata`。原因：这些字段无跨协议转换语义，强类型化无收益）
- 不得做无关重构（"while I'm here" 禁止）
- 不得新增 error code（除非 gap 明确要求）
- 不得修改 config/default.toml 或配置结构
- 不得修改 soak harness 代码（只跑验证，不改 harness）
- 不得添加额外 observability（仅 lossy counter）

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** - ALL verification is agent-executed. No exceptions.

### Test Decision
- **Infrastructure exists**: YES
- **Automated tests**: Tests-after
- **Framework**: `cargo test` (Rust built-in + 已有 test 基础设施)
- **策略**: 每个 task 实现后补 round-trip parse→IR→render 测试

### QA Policy
Every task MUST include agent-executed QA scenarios.
Evidence saved to `.omo/evidence/task-{N}-{scenario-slug}.{ext}`.

- **Core codec**: Use Bash — `cargo test -p gw-codec`, `cargo test -p gw-core`
- **Integration**: Use Bash — `cargo test --workspace`, `cargo clippy`, `bash scripts/check-invariants.sh`
- **Soak**: Use Bash — `make e2e-soak-smoke`

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Foundation — sequential: IR types then WireCapabilities):
├── Task 1: IR 类型全量扩展 [deep]
└── Task 2: WireCapabilities + adapt_lossy 框架扩展 (depends: 1) [deep]

Wave 2 (P0 — 功能断裂修复, MAX PARALLEL):
├── Task 3: OpenAI Chat User 多模态 wire + parse/render (depends: 1) [unspecified-high]
├── Task 4: OpenAI Chat developer 角色 (depends: 1) [quick]
├── Task 5: Anthropic 服务端工具 block 建模 (depends: 1) [deep]
├── Task 6: Gemini GenerationConfig 扩展 (depends: 1) [unspecified-high]
└── Task 7: OpenAI Chat refusal + annotations wire (depends: 1) [unspecified-high]

Wave 3 (P1 — 有损转换修复, MAX PARALLEL):
├── Task 8: Anthropic redacted_thinking + pause_turn + stop reasons (depends: 1, 5) [unspecified-high]
├── Task 9: Gemini thinking parts + thoughtsTokenCount (depends: 1, 2) [unspecified-high]
├── Task 10: OpenAI Responses output item 扩展 (depends: 1) [deep]
├── Task 11: Anthropic document + search_result input blocks (depends: 1) [unspecified-high]
├── Task 12: OpenAI reasoning_effort + service_tier (depends: 1) [unspecified-high]
├── Task 13: OpenAI audio modality wire (depends: 1) [unspecified-high]
├── Task 14: Anthropic thinking config 新选项 (depends: 1, 5) [quick]
└── Task 15: OpenAI structured output 确认 + response_format 完善 (depends: 1) [quick]

Wave 4 (P2 — 精度损失修复, MAX PARALLEL):
├── Task 16: adapt_lossy 全矩阵 + finish_reason 扩展 (depends: 2, 8, 9, 12) [deep]
├── Task 17: Anthropic cache TTL + inference_geo + service_tier (depends: 1) [unspecified-high]
├── Task 18: Gemini executableCode + codeExecutionResult + grounding (depends: 1) [unspecified-high]
├── Task 19: OpenAI web_search + prediction + custom tool (depends: 1) [unspecified-high]
├── Task 20: Usage 细分字段全协议扩展 (depends: 1, 2) [unspecified-high]
├── Task 21: Anthropic citations_delta + MCP connector blocks (depends: 1, 5) [unspecified-high]
└── Task 22: OpenAI Responses streaming 50+ event types (depends: 10) [deep]

Wave 5 (Verification + Docs):
├── Task 23: 全量 round-trip 测试 + integration (depends: all) [deep]
├── Task 24: docs/architecture.md + docs/go-contracts/ 同步 (depends: all) [writing]
└── Task 25: make e2e-soak-smoke 验证 (depends: 23) [quick]

Wave FINAL (After ALL tasks — 4 parallel reviews, then user okay):
├── Task F1: Plan compliance audit (oracle)
├── Task F2: Code quality review (unspecified-high)
├── Task F3: Real manual QA (unspecified-high)
└── Task F4: Scope fidelity check (deep)
-> Present results -> Get explicit user okay
```

### Dependency Matrix

| Task | Depends On | Blocks | Wave |
|------|-----------|--------|------|
| 1 | - | 2-22 | 1 |
| 2 | 1 | 9, 16, 20 | 1 |
| 3 | 1 | 23 | 2 |
| 4 | 1 | 23 | 2 |
| 5 | 1 | 8, 14, 21 | 2 |
| 6 | 1 | 23 | 2 |
| 7 | 1 | 23 | 2 |
| 8 | 1, 5 | 16 | 3 |
| 9 | 1, 2 | 16 | 3 |
| 10 | 1 | 22 | 3 |
| 11 | 1 | 23 | 3 |
| 12 | 1 | 16 | 3 |
| 13 | 1 | 23 | 3 |
| 14 | 1, 5 | 23 | 3 |
| 15 | 1 | 23 | 3 |
| 16 | 2, 8, 9, 12 | 23 | 4 |
| 17 | 1 | 23 | 4 |
| 18 | 1 | 23 | 4 |
| 19 | 1 | 23 | 4 |
| 20 | 1, 2 | 23 | 4 |
| 21 | 1, 5 | 23 | 4 |
| 22 | 10 | 23 | 4 |
| 23 | all | 25 | 5 |
| 24 | all | - | 5 |
| 25 | 23 | F1-F4 | 5 |

### Agent Dispatch Summary

- **Wave 1**: **2** — T1 → `deep`, T2 → `deep`
- **Wave 2**: **5** — T3 → `unspecified-high`, T4 → `quick`, T5 → `deep`, T6 → `unspecified-high`, T7 → `unspecified-high`
- **Wave 3**: **8** — T8-T9 → `unspecified-high`, T10 → `deep`, T11-T13 → `unspecified-high`, T14-T15 → `quick`
- **Wave 4**: **7** — T16 → `deep`, T17-T21 → `unspecified-high`, T22 → `deep`
- **Wave 5**: **3** — T23 → `deep`, T24 → `writing`, T25 → `quick`
- **FINAL**: **4** — F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`, F4 → `deep`

---

## TODOs

- [x] 1. IR 类型全量扩展 — RequestIR + ResponseIR + StreamEventIR + FinishReasonIR + UsageIR

  **What to do**:
  - 在 `crates/gw-core/src/ir.rs` 中扩展以下类型：
  - **MessageIR**: 新增 `Developer { content: ContentIR<'a> }` 变体（Gap #4 的 IR 基础）
  - **RequestIR**: 新增字段：
    - `reasoning_config: Option<ReasoningConfig>` — 统一三家的推理控制（Gap #14）
    - `audio_config: Option<AudioConfig>` — 音频模态控制（Gap #16）
    - `service_tier: Option<Cow<'a, str>>` — 服务层级（Gap #21）
    - `web_search: Option<Box<RawValue>>` — web 搜索配置透传（Gap #22）
    - `prediction: Option<Box<RawValue>>` — speculative decoding 透传（Gap #23）
  - **新增 struct**:
    - `ReasoningConfig { effort: Option<Cow<'a, str>>, budget_tokens: Option<u32> }` — 统一 OpenAI reasoning_effort / Anthropic thinking.budget_tokens / Gemini thinkingBudget
    - `AudioConfig { voice: Option<Cow<'a, str>>, format: Option<Cow<'a, str>>, modalities: Option<Vec<Cow<'a, str>>> }`
  - **FinishReasonIR**: 新增 `Refusal` 变体（Gap #5）
  - **UsageIR**: 新增字段：
    - `audio_tokens_in: Option<u32>`, `audio_tokens_out: Option<u32>` — OpenAI 音频 token
    - `accepted_prediction_tokens: Option<u32>`, `rejected_prediction_tokens: Option<u32>` — OpenAI prediction
    - `thoughts_tokens: Option<u32>` — Gemini thinking token（Gap #27）
    - `cache_creation_tokens: Option<u32>` — Anthropic cache creation
    - `cache_creation_1h_tokens: Option<u32>`, `cache_creation_5m_tokens: Option<u32>` — Anthropic cache TTL 分级
    - `server_tool_web_search_requests: Option<u32>`, `server_tool_web_fetch_requests: Option<u32>` — Anthropic 服务端工具 usage
    - `service_tier: Option<Cow<'static, str>>` — Anthropic/OpenAI service tier
    - `inference_geo: Option<Cow<'static, str>>` — Anthropic 地理约束
  - **ResponseIR**: 新增字段：
    - `refusal: Option<Cow<'a, str>>` — assistant refusal 内容（Gap #6）
    - `annotations: Option<Vec<AnnotationIR<'a>>>` — 引用标注（Gap #7）
    - `server_tool_blocks: Option<Vec<ServerToolBlockIR<'a>>>` — 服务端工具 block（Gap #3 的 IR 基础）
    - `redacted_thinking: Option<Vec<RedactedThinkingIR<'a>>>` — 不透明 thinking block（Gap #8）
    - `service_tier: Option<Cow<'a, str>>` — 服务层级（Gap #21）
  - **新增 struct/enum**:
    - `AnnotationIR<'a> { kind: Cow<'a, str>, url: Option<Cow<'a, str>>, title: Option<Cow<'a, str>>, start_index: Option<u32>, end_index: Option<u32> }`
    - `ServerToolBlockIR<'a> { kind: Cow<'a, str>, id: Cow<'a, str>, name: Cow<'a, str>, input: Box<RawValue>, extra: serde_json::Map<String, serde_json::Value> }` — typed wrapper + opaque inner
    - `RedactedThinkingIR<'a> { data: Cow<'a, str> }` — opaque base64 data
  - **StreamEventIR**: 新增变体：
    - `RefusalDelta { text: Cow<'a, str> }` — 流式 refusal（Gap #6）
    - `CitationsDelta { data: Cow<'a, str> }` — Anthropic citations（Gap #25）
    - `ServerToolStart { index: u32, kind: Cow<'a, str>, id: Cow<'a, str>, name: Cow<'a, str> }` — 服务端工具流式开启
    - `ServerToolDelta { index: u32, data: Cow<'a, str> }` — 服务端工具流式数据
  - **MetadataIR**: 新增 `anthropic_thinking` 扩展字段支持 `display: Option<Cow<'a, str>>` 和 `adaptive: Option<bool>`（Gap #15）
  - 确保所有新类型实现 `Clone + Debug`，新 struct 如果用于 response 侧需考虑 `Default`
  - 确保 `ResponseIR::default()` 中所有新字段为 `None`
  - 保持 `Mutations` / `ForcedMutations` 的 bit flag 结构——如需新 mutation bit 用下一个空闲 bit

  **Must NOT do**:
  - 不修改现有字段的类型或语义
  - 不引入 `serde_json::Value`（ServerToolBlockIR.extra 用 `serde_json::Map` 例外）
  - 不在此 task 修改任何 codec 文件——只改 `crates/gw-core/src/ir.rs`

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 涉及核心 IR 类型设计，需要理解全局 hub-and-spoke 架构
  - **Skills**: []
  - **Skills Evaluated but Omitted**:
    - None applicable for pure Rust struct definition

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Task 2)
  - **Parallel Group**: Wave 1
  - **Blocks**: Tasks 3-22
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/gw-core/src/ir.rs:15-58` — `RequestIR` 现有结构，新字段按此 pattern 追加
  - `crates/gw-core/src/ir.rs:87-92` — `ContentPart` 已有 Audio/File 变体，无需再加
  - `crates/gw-core/src/ir.rs:193-218` — `ResponseIR` 现有结构 + Default impl
  - `crates/gw-core/src/ir.rs:223-231` — `FinishReasonIR` 现有变体
  - `crates/gw-core/src/ir.rs:234-240` — `UsageIR` 现有字段
  - `crates/gw-core/src/ir.rs:243-273` — `StreamEventIR` 现有变体
  - `crates/gw-core/src/ir.rs:155-158` — `AnthropicThinking` 现有 struct，需扩展 display/adaptive

  **WHY Each Reference Matters**:
  - RequestIR: 新字段必须对齐现有 `Option<T>` + `Default::None` pattern
  - ResponseIR: Default impl 必须同步更新，新字段全 None
  - FinishReasonIR: 新变体 Refusal 需配合 cross_render.rs 的 map_finish_reason
  - UsageIR: 新字段全 Option，需保持 Serialize/Deserialize derive

  **Acceptance Criteria**:
  - [ ] `cargo build -p gw-core` 编译通过
  - [ ] `cargo test -p gw-core` 全通过
  - [ ] 所有新类型实现 Clone + Debug
  - [ ] `ResponseIR::default()` 所有新字段为 None

  **QA Scenarios**:

  ```
  Scenario: IR 扩展后编译验证
    Tool: Bash
    Preconditions: 当前 workspace clean
    Steps:
      1. cargo build -p gw-core 2>&1
      2. cargo test -p gw-core -- ir 2>&1
    Expected Result: 编译 0 error，测试全 PASS
    Failure Indicators: 任何编译 error 或 test failure
    Evidence: .omo/evidence/task-1-ir-build.txt

  Scenario: 新 FinishReasonIR::Refusal 构造验证
    Tool: Bash
    Preconditions: Task 1 实现完成
    Steps:
      1. 在 ir_v2_tests 模块中添加 test：构造 FinishReasonIR::Refusal，assert matches!
      2. cargo test -p gw-core -- finish_reason 2>&1
    Expected Result: 测试通过
    Evidence: .omo/evidence/task-1-refusal-variant.txt

  Scenario: 新 UsageIR 字段 default 值验证
    Tool: Bash
    Preconditions: Task 1 实现完成
    Steps:
      1. 在 ir_v2_tests 中添加 test：UsageIR::default() 新字段全 None
      2. cargo test -p gw-core -- usage_ir 2>&1
    Expected Result: 全 PASS
    Evidence: .omo/evidence/task-1-usage-defaults.txt
  ```

  **Commit**: YES (groups with Task 2 in Wave 1 commit)
  - Message: `feat(core): extend IR types for 30 codec gap coverage`
  - Files: `crates/gw-core/src/ir.rs`
  - Pre-commit: `cargo test -p gw-core && cargo clippy -p gw-core -- -D warnings`

- [x] 2. WireCapabilities + adapt_lossy 框架扩展

  **What to do**:
  - 在 `crates/gw-codec/src/cross_render.rs` 中：
  - **WireCapabilities 扩展**（新增 bool 字段）：
    - `supports_refusal: bool` — OpenAI Chat/Responses 支持，Anthropic/Gemini 不支持
    - `supports_annotations: bool` — OpenAI Chat 支持
    - `supports_server_tool_blocks: bool` — Anthropic 支持
    - `supports_redacted_thinking: bool` — Anthropic 支持
    - `supports_audio: bool` — OpenAI Chat 支持
    - `supports_document_blocks: bool` — Anthropic 支持
    - `supports_service_tier: bool` — OpenAI/Anthropic 支持
    - `supports_reasoning_config: bool` — 三家都有各自实现
    - `supports_grounding: bool` — Gemini 支持
    - `supports_citations: bool` — Anthropic 支持
    - `supports_mcp: bool` — Anthropic 支持
  - **更新 capabilities() 矩阵**：为每个 WireFormat 设置正确的 bool 值
  - **扩展 adapt_lossy()**：为每个新 WireCapabilities 字段添加 lossy drop 逻辑：
    - 如果 `!to_caps.supports_X && ir.X.is_some()` → `LossyField::drop(...)` + `ir.X = None`
    - 对 ResponseIR 新字段：refusal, annotations, server_tool_blocks, redacted_thinking, service_tier
    - 对 UsageIR 新字段：所有新 Option 字段按目标能力 drop
  - **扩展 map_finish_reason()**：
    - `Refusal` → 对不支持 refusal 的目标映射为 `Stop` + lossy log
  - 补充测试：adapt_lossy_matrix 测试覆盖所有新字段的 4×4 wire format 组合

  **Must NOT do**:
  - 不修改 `CrossRender::run()` 的调用接口
  - 不修改 `LossyField::drop()` 的签名
  - 不修改现有 adapt_lossy 逻辑（thinking/reasoning/reasoning_tokens）

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 跨协议能力矩阵设计需要精确理解 4 个 wire format 的语义差异
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Wave 1 (sequential after Task 1)
  - **Blocks**: Tasks 9, 16, 20
  - **Blocked By**: Task 1 (需要 Task 1 的 IR 类型定义完成才能编写 adapt_lossy 逻辑)

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/cross_render.rs:97-104` — 现有 `WireCapabilities` struct，按此 pattern 追加 bool 字段
  - `crates/gw-codec/src/cross_render.rs:52-87` — 现有 `capabilities()` 矩阵，为新字段设置每个 wire format 的值
  - `crates/gw-codec/src/cross_render.rs:144-173` — 现有 `adapt_lossy()` pattern: check cap → LossyField::drop → set None
  - `crates/gw-codec/src/cross_render.rs:176-198` — 现有 `map_finish_reason()` pattern
  - `crates/gw-codec/src/cross_render.rs:275-300` — 现有 matrix test pattern，为新字段复制此 pattern

  **WHY Each Reference Matters**:
  - WireCapabilities: 新 bool 字段必须对齐现有命名 pattern `supports_X`
  - capabilities() 矩阵: 每个 WireFormat 的 true/false 设置决定跨协议 lossy 行为——错误设置会导致数据丢失或 panic
  - adapt_lossy: 新字段的 drop 逻辑必须严格遵循 check→log→set-None 三步 pattern
  - map_finish_reason: Refusal 变体需要跨协议降级规则

  **Acceptance Criteria**:
  - [ ] `cargo build -p gw-codec` 编译通过
  - [ ] `cargo test -p gw-codec -- cross_render` 全通过
  - [ ] 新 WireCapabilities 字段在 capabilities() 矩阵中全部有值

  **QA Scenarios**:

  ```
  Scenario: adapt_lossy 新字段 drop 验证
    Tool: Bash
    Preconditions: Task 1 + Task 2 实现完成
    Steps:
      1. 在 cross_render::tests 中添加 test：构造含 refusal + annotations 的 ResponseIR
      2. CrossRender{from: Anthropic, to: OpenAiChat}.adapt_lossy()
      3. assert annotations 保留（OpenAI Chat 支持）
      4. CrossRender{from: OpenAiChat, to: GeminiGenerate}.adapt_lossy()
      5. assert refusal + annotations 被 drop（Gemini 不支持）
      6. cargo test -p gw-codec -- adapt_lossy 2>&1
    Expected Result: 全 PASS
    Evidence: .omo/evidence/task-2-adapt-lossy-matrix.txt

  Scenario: map_finish_reason Refusal 降级
    Tool: Bash
    Preconditions: Task 2 实现完成
    Steps:
      1. 测试 map_finish_reason(OpenAiChat, GeminiGenerate, Refusal)
      2. assert 返回 Stop
      3. cargo test -p gw-codec -- finish_reason 2>&1
    Expected Result: Refusal → Stop with lossy log
    Evidence: .omo/evidence/task-2-refusal-degradation.txt
  ```

  **Commit**: YES (groups with Task 1 in Wave 1 commit)
  - Message: `feat(codec): extend WireCapabilities and adapt_lossy for full gap coverage`
  - Files: `crates/gw-codec/src/cross_render.rs`
  - Pre-commit: `cargo test -p gw-codec -- cross_render && cargo clippy -p gw-codec -- -D warnings`

- [x] 3. OpenAI Chat User 多模态 wire + parse/render (Gap #1)

  **What to do**:
  - 在 `crates/gw-codec/src/openai/wire.rs` 中：
    - 将 `OpenAiMessage::User { content: Cow<'a, str> }` 改为 `User { content: OpenAiUserContent<'a> }`
    - 新增 `enum OpenAiUserContent<'a> { Text(Cow<'a, str>), Parts(Vec<OpenAiContentPart<'a>>) }` — 需要自定义 Deserialize 处理 string | array
    - 新增 `enum OpenAiContentPart<'a> { Text { text: Cow<'a, str> }, ImageUrl { image_url: OpenAiImageUrl<'a> }, InputAudio { input_audio: OpenAiInputAudio<'a> }, File { file: OpenAiFileRef<'a> } }`
    - 新增 `struct OpenAiImageUrl<'a> { url: Cow<'a, str>, detail: Option<Cow<'a, str>> }`
    - 新增 `struct OpenAiInputAudio<'a> { data: Cow<'a, str>, format: Cow<'a, str> }`
    - 新增 `struct OpenAiFileRef<'a> { file_data: Option<Cow<'a, str>>, file_id: Option<Cow<'a, str>>, filename: Option<Cow<'a, str>> }`
  - 在 `crates/gw-codec/src/openai/via.rs` 中更新 parse/render：
    - `parse_request`: User content string → `ContentIR::Text`; content parts → `ContentIR::Parts(SmallVec<ContentPart>)`
    - `render_request`: `ContentIR::Text` → string; `ContentIR::Parts` → array of content parts
    - 确保 round-trip 保持
  - 在 `crates/gw-codec/src/openai/codec.rs` 中如有 User message 特殊处理需同步更新

  **Must NOT do**:
  - 不修改 Assistant/System/Tool message 的 content 类型
  - 不修改 same-protocol passthrough 逻辑

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 涉及自定义 serde Deserialize + wire 类型变更
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with Tasks 4, 5, 6, 7)
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/openai/wire.rs:55-76` — 现有 `OpenAiMessage` enum，User 变体需修改
  - `crates/gw-codec/src/openai/via.rs` — parse_request / render_request 函数
  - `crates/gw-codec/src/anthropic/wire.rs:67-103` — Anthropic `AnthropicContent` 已支持多种 content block 的 serde pattern 可参考

  **WHY Each Reference Matters**:
  - OpenAiMessage::User: 这是要修改的核心类型——从 String 改为 union type
  - via.rs: parse/render 必须同步更新以正确映射 IR ContentPart ↔ wire OpenAiContentPart
  - Anthropic content: 参考其自定义 Deserialize 处理 tagged enum 的 pattern

  **Acceptance Criteria**:
  - [ ] `cargo build -p gw-codec` 编译通过
  - [ ] 新 fixture JSON 包含 image_url content part 的 request 能成功 parse
  - [ ] round-trip: parse → IR → render → re-parse 结果一致

  **QA Scenarios**:

  ```
  Scenario: 多模态 User message parse
    Tool: Bash
    Preconditions: Task 1 + Task 3 实现完成
    Steps:
      1. 创建 fixture: {"role":"user","content":[{"type":"text","text":"describe this"},{"type":"image_url","image_url":{"url":"https://example.com/img.png","detail":"high"}}]}
      2. 在测试中 deserialize 此 JSON 为 OpenAiMessage
      3. assert content 是 Parts variant，包含 Text + ImageUrl
      4. cargo test -p gw-codec -- openai_multimodal 2>&1
    Expected Result: parse 成功，类型正确
    Evidence: .omo/evidence/task-3-multimodal-parse.txt

  Scenario: 纯文本 User message 向后兼容
    Tool: Bash
    Preconditions: Task 3 实现完成
    Steps:
      1. 原有 fixture: {"role":"user","content":"hello"}
      2. deserialize 为 OpenAiMessage
      3. assert content 是 Text variant
    Expected Result: 向后兼容，string content 仍然正常
    Evidence: .omo/evidence/task-3-text-compat.txt
  ```

  **Commit**: YES (groups with Wave 2)
  - Message: `feat(codec/openai): support multimodal user content parts`
  - Files: `crates/gw-codec/src/openai/wire.rs`, `crates/gw-codec/src/openai/via.rs`
  - Pre-commit: `cargo test -p gw-codec -- openai`

- [x] 4. OpenAI Chat developer 角色 (Gap #4)

  **What to do**:
  - 在 `crates/gw-codec/src/openai/wire.rs` 中 `OpenAiMessage` enum 新增 `Developer { content: Cow<'a, str> }` 变体
  - 在 `crates/gw-codec/src/openai/via.rs` 中：
    - `parse_request`: `Developer` → `MessageIR::Developer { content: ContentIR::Text(content) }`
    - `render_request`: `MessageIR::Developer` → `OpenAiMessage::Developer { content }`
  - 在其他 codec 的 `via.rs` 中：
    - Anthropic render: `MessageIR::Developer` → 当作 system prompt 处理（追加到 system 参数）
    - Gemini render: `MessageIR::Developer` → 追加到 `system_instruction.parts`
    - 其他 codec parse: 不会遇到 developer（只有 OpenAI 产生），无需处理

  **Must NOT do**:
  - 不改变现有 system role 的行为

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 简单的 enum 变体添加 + 映射
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/openai/wire.rs:55-76` — OpenAiMessage enum
  - `crates/gw-codec/src/openai/via.rs` — parse/render System 的 pattern 可直接复用
  - `crates/gw-codec/src/anthropic/via.rs` — system prompt 映射 pattern
  - `crates/gw-codec/src/gemini/via.rs` — system_instruction 映射 pattern

  **Acceptance Criteria**:
  - [ ] `cargo build -p gw-codec` 编译通过
  - [ ] Developer message 在 OpenAI wire 上 round-trip 正确
  - [ ] Developer message cross-render 到 Anthropic/Gemini 映射为 system prompt

  **QA Scenarios**:

  ```
  Scenario: Developer role parse + render
    Tool: Bash
    Steps:
      1. fixture: {"role":"developer","content":"You are a helpful assistant"}
      2. parse → MessageIR::Developer
      3. render → {"role":"developer","content":"You are a helpful assistant"}
      4. cargo test -p gw-codec -- developer 2>&1
    Expected Result: round-trip 正确
    Evidence: .omo/evidence/task-4-developer-roundtrip.txt
  ```

  **Commit**: YES (groups with Wave 2)
  - Message: `feat(codec/openai): add developer role support`
  - Files: `crates/gw-codec/src/openai/wire.rs`, `crates/gw-codec/src/openai/via.rs`, `crates/gw-codec/src/anthropic/via.rs`, `crates/gw-codec/src/gemini/via.rs`
  - Pre-commit: `cargo test -p gw-codec`

- [x] 5. Anthropic 服务端工具 block 完整建模 (Gap #3)

  **What to do**:
  - 在 `crates/gw-codec/src/anthropic/wire.rs` 中 `AnthropicContent` enum 新增 8 个变体：
    - `ServerToolUse { id, name, input: Box<RawValue>, #[serde(flatten)] extra: serde_json::Map }`
    - `WebSearchToolResult { id, search_results: Vec<...>, #[serde(flatten)] extra: serde_json::Map }`
    - `WebFetchToolResult { id, content: ..., #[serde(flatten)] extra: serde_json::Map }`
    - `CodeExecutionToolResult { id, output: ..., #[serde(flatten)] extra: serde_json::Map }`
    - `BashCodeExecutionToolResult { id, output: ..., #[serde(flatten)] extra: serde_json::Map }`
    - `TextEditorCodeExecutionToolResult { id, output: ..., #[serde(flatten)] extra: serde_json::Map }`
    - `ToolSearchToolResult { id, results: ..., #[serde(flatten)] extra: serde_json::Map }`
    - `ContainerUpload { file_id: Cow<'a, str> }`
  - 每个变体使用 typed wrapper：已知关键字段（id, name）强类型化，vendor-specific 内部 payload 用 `serde_json::Map<String, serde_json::Value>` via `#[serde(flatten)]`
  - 在 `crates/gw-codec/src/anthropic/via.rs` 中更新 parse/render：
    - parse_response: 新 content block → `ServerToolBlockIR` in ResponseIR
    - render_response: `ServerToolBlockIR` → 对应的 AnthropicContent 变体
  - 在其他 codec 的 cross-render: server_tool_blocks 对非 Anthropic 目标走 lossy drop（由 Task 2 的 adapt_lossy 处理）

  **Must NOT do**:
  - 不尝试完全建模每种工具结果的内部结构——使用 typed wrapper + opaque inner
  - 不修改现有 ToolUse / ToolResult 变体

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 8 种新 content block 的 serde tagged enum 设计需要精确处理
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: Tasks 8, 14, 21
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/anthropic/wire.rs:67-103` — 现有 `AnthropicContent` enum，新变体按此 tagged enum pattern 追加
  - `crates/gw-codec/src/anthropic/via.rs` — parse/render content block 的 match 分支
  - `crates/gw-core/src/ir.rs` — `ServerToolBlockIR` struct（Task 1 新增）

  **WHY Each Reference Matters**:
  - AnthropicContent: serde `#[serde(tag = "type")]` tagged enum pattern 必须对齐
  - via.rs: parse/render 的 match arm 必须覆盖所有新变体（否则 #[deny(unreachable_patterns)] 编译失败）

  **Acceptance Criteria**:
  - [ ] `cargo build -p gw-codec` 编译通过
  - [ ] 8 种新 content block 的 JSON 能成功 parse
  - [ ] round-trip parse → IR → render → re-parse 不丢字段

  **QA Scenarios**:

  ```
  Scenario: ServerToolUse block parse
    Tool: Bash
    Steps:
      1. fixture: {"type":"server_tool_use","id":"srvtu_01","name":"web_search_20260209","input":{"query":"rust programming"}}
      2. deserialize 为 AnthropicContent
      3. assert 是 ServerToolUse variant
      4. serialize 回 JSON，assert 包含 type/id/name/input
    Expected Result: round-trip 正确
    Evidence: .omo/evidence/task-5-server-tool-parse.txt

  Scenario: Unknown future block 不 panic
    Tool: Bash
    Steps:
      1. fixture: {"type":"unknown_future_block","id":"xxx","data":"..."}
      2. deserialize AnthropicContent
      3. 策略：使用 `#[serde(other)]` 属性将未知 type tag 映射到一个 unit variant `Unknown`（不携带任何数据，因此不引入 `serde_json::Value`）。在 via.rs parse 中遇到 `Unknown` variant 时直接 skip（不放入 IR），log 一条 warn。Gateway 设计原则: fail-closed (见 docs/architecture.md §Deliberate behaviours)，但对上游 response 中的未知 content block 应当 skip 而非 reject 整个 response（不丢弃已知 block）。
    Expected Result: 不 panic，unknown block 被 skip（不出现在 IR 中），已知 block 正常传递
    Evidence: .omo/evidence/task-5-unknown-block.txt
  ```

  **Commit**: YES (groups with Wave 2)
  - Message: `feat(codec/anthropic): model 8 server tool block types`
  - Files: `crates/gw-codec/src/anthropic/wire.rs`, `crates/gw-codec/src/anthropic/via.rs`
  - Pre-commit: `cargo test -p gw-codec -- anthropic`

- [x] 6. Gemini GenerationConfig 扩展 (Gap #2)

  **What to do**:
  - 在 `crates/gw-codec/src/gemini/wire.rs` 中 `GeminiGenerationConfig` 新增字段：
    - `top_p: Option<f32>`
    - `top_k: Option<u32>`
    - `stop_sequences: Option<Vec<Cow<'a, str>>>` — 需要给 struct 加生命周期参数
    - `candidate_count: Option<u32>`
    - `presence_penalty: Option<f32>`
    - `frequency_penalty: Option<f32>`
    - `seed: Option<u64>`
    - `response_mime_type: Option<Cow<'a, str>>`
    - `response_schema: Option<Box<RawValue>>`
    - `thinking_config: Option<GeminiThinkingConfig>` — 新 struct `{ thinking_budget: Option<u32>, thinking_level: Option<Cow<'a, str>> }`
  - 所有新字段 `#[serde(default, skip_serializing_if = "Option::is_none")]` + `rename_all = "camelCase"`
  - 在 `crates/gw-codec/src/gemini/via.rs` 中更新 parse/render：
    - parse_request: IR `top_p`/`top_k`/`stop`/`seed` 等 → GeminiGenerationConfig 对应字段
    - render_request: GeminiGenerationConfig → IR 字段

  **Must NOT do**:
  - 不修改 GeminiResponse / GeminiCandidate 结构（response 侧在其他 task）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: 多字段添加 + serde camelCase 对齐
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/gemini/wire.rs:139-144` — 现有 `GeminiGenerationConfig` 只有 2 字段
  - `crates/gw-codec/src/gemini/via.rs` — parse/render request 函数
  - `crates/gw-core/src/ir.rs:15-34` — RequestIR 中 top_p/top_k/stop/seed 等字段位置

  **Acceptance Criteria**:
  - [ ] `cargo build -p gw-codec` 编译通过
  - [ ] IR top_p/top_k/stop/seed round-trip 到 Gemini wire 正确

  **QA Scenarios**:

  ```
  Scenario: GenerationConfig 全字段 round-trip
    Tool: Bash
    Steps:
      1. fixture Gemini request JSON 含 generationConfig: {maxOutputTokens:100, temperature:0.7, topP:0.9, topK:40, stopSequences:["END"], seed:42}
      2. parse → RequestIR
      3. assert IR.top_p == Some(0.9), IR.top_k == Some(40), IR.stop == Some(["END"]), IR.seed == Some(42)
      4. render → Gemini JSON
      5. assert 输出含所有字段
    Expected Result: 全 round-trip
    Evidence: .omo/evidence/task-6-genconfig-roundtrip.txt
  ```

  **Commit**: YES (groups with Wave 2)
  - Message: `feat(codec/gemini): extend GenerationConfig to 12 fields`
  - Files: `crates/gw-codec/src/gemini/wire.rs`, `crates/gw-codec/src/gemini/via.rs`
  - Pre-commit: `cargo test -p gw-codec -- gemini`

- [x] 7. OpenAI Chat refusal + annotations wire (Gap #6, #7)

  **What to do**:
  - 在 `crates/gw-codec/src/openai/wire.rs` 中：
    - `OpenAiMessage::Assistant` 新增 `refusal: Option<Cow<'a, str>>` 字段
    - `OpenAiResponse` choices message 新增 `annotations: Option<Vec<OpenAiAnnotation<'a>>>`
    - 新增 `struct OpenAiAnnotation<'a> { #[serde(rename = "type")] ty: Cow<'a, str>, url_citation: Option<OpenAiUrlCitation<'a>> }`
    - 新增 `struct OpenAiUrlCitation<'a> { start_index: u32, end_index: u32, title: Cow<'a, str>, url: Cow<'a, str> }`
  - 在 stream chunk delta 中新增 `refusal: Option<Cow<'a, str>>` 字段
  - 在 `crates/gw-codec/src/openai/via.rs` 中：
    - parse_response: `message.refusal` → `ResponseIR.refusal`; `message.annotations` → `ResponseIR.annotations`
    - render_response: 反向映射
    - parse_stream_chunk: `delta.refusal` → `StreamEventIR::RefusalDelta`
    - render_stream_chunk: `RefusalDelta` → delta.refusal

  **Must NOT do**:
  - 不修改 User/System/Tool message 结构

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:

  **Pattern References**:
  - `crates/gw-codec/src/openai/wire.rs:63-69` — Assistant message 结构
  - `crates/gw-codec/src/openai/wire.rs:123-145` — Response/Choice 结构

  **Acceptance Criteria**:
  - [ ] refusal 字段 round-trip 正确
  - [ ] annotations 数组 round-trip 正确
  - [ ] stream refusal delta round-trip 正确

  **QA Scenarios**:

  ```
  Scenario: Assistant refusal parse
    Tool: Bash
    Steps:
      1. fixture: response JSON 含 message.refusal = "I cannot help with that"
      2. parse → ResponseIR
      3. assert ir.refusal == Some("I cannot help with that")
    Expected Result: refusal 正确传递
    Evidence: .omo/evidence/task-7-refusal-parse.txt

  Scenario: Annotations round-trip
    Tool: Bash
    Steps:
      1. fixture: response JSON 含 annotations: [{type:"url_citation",url_citation:{start_index:0,end_index:10,title:"Example",url:"https://example.com"}}]
      2. parse → IR → render → 比对
    Expected Result: annotations round-trip 完整
    Evidence: .omo/evidence/task-7-annotations-roundtrip.txt
  ```

  **Commit**: YES (groups with Wave 2)
  - Message: `feat(codec/openai): add refusal and annotations wire support`
  - Files: `crates/gw-codec/src/openai/wire.rs`, `crates/gw-codec/src/openai/via.rs`
  - Pre-commit: `cargo test -p gw-codec -- openai`

- [x] 8. Anthropic redacted_thinking + 新 stop reasons (Gap #8, #9)

  **What to do**:
  - 在 `crates/gw-codec/src/anthropic/wire.rs` 中 `AnthropicContent` 新增：
    - `RedactedThinking { data: Cow<'a, str> }` 变体
  - 在 Anthropic stop_reason parse 逻辑中确保 `pause_turn`, `refusal`, `model_context_window_exceeded` 映射到正确的 `FinishReasonIR` 变体：
    - `pause_turn` → `Other("pause_turn")`
    - `refusal` → `FinishReasonIR::Refusal`
    - `model_context_window_exceeded` → `Other("model_context_window_exceeded")`
  - 在 via.rs parse_response 中：`RedactedThinking` block → `ResponseIR.redacted_thinking`
  - 在 via.rs render_response 中：`ResponseIR.redacted_thinking` → `AnthropicContent::RedactedThinking`

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 16
  - **Blocked By**: Tasks 1, 5

  **References**:
  - `crates/gw-codec/src/anthropic/wire.rs:67-103` — AnthropicContent enum
  - `crates/gw-codec/src/anthropic/codec.rs` — stop_reason parse 逻辑

  **Acceptance Criteria**:
  - [ ] redacted_thinking block round-trip 正确
  - [ ] `pause_turn` stop reason parse 为 `Other("pause_turn")`
  - [ ] `refusal` stop reason parse 为 `FinishReasonIR::Refusal`

  **QA Scenarios**:

  ```
  Scenario: RedactedThinking block parse
    Tool: Bash
    Steps:
      1. fixture: {"type":"redacted_thinking","data":"base64opaquecontent=="}
      2. parse → assert redacted_thinking 填充
    Expected Result: data 保留
    Evidence: .omo/evidence/task-8-redacted-thinking.txt

  Scenario: pause_turn stop reason
    Tool: Bash
    Steps:
      1. response fixture with stop_reason: "pause_turn"
      2. parse → FinishReasonIR::Other("pause_turn")
    Expected Result: 不 panic，映射正确
    Evidence: .omo/evidence/task-8-pause-turn.txt
  ```

  **Commit**: YES (groups with Wave 3)
  - Files: `crates/gw-codec/src/anthropic/wire.rs`, `crates/gw-codec/src/anthropic/via.rs`, `crates/gw-codec/src/anthropic/codec.rs`

- [x] 9. Gemini thinking parts + thoughtsTokenCount (Gap #10)

  **What to do**:
  - 在 `crates/gw-codec/src/gemini/wire.rs` 中 `GeminiPart` enum 新增 metadata 字段：
    - 每个 Part 变体需支持 `thought: Option<bool>` 和 `thought_signature: Option<Cow<'a, str>>` metadata（Gemini Part-level metadata）
    - 实现方式：新增 wrapper struct 或在 Text 变体中增加可选字段
  - 在 `GeminiUsage` 中新增 `thoughts_token_count: Option<u32>`
  - 在 via.rs 中：
    - parse_response: thought=true 的 Part → `ThinkingBlockIR` in `ResponseIR.thinking`
    - render_response: `ResponseIR.thinking` → thought=true 的 Text Part
    - parse usage: `thoughts_token_count` → `UsageIR.thoughts_tokens`
  - 更新 `WireCapabilities::GeminiGenerate`: `supports_thinking: true`（Gemini 现在支持 thinking）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 16
  - **Blocked By**: Tasks 1, 2

  **References**:
  - `crates/gw-codec/src/gemini/wire.rs:61-95` — GeminiPart enum
  - `crates/gw-codec/src/gemini/wire.rs:187-198` — GeminiUsage struct
  - `crates/gw-codec/src/cross_render.rs:78-85` — GeminiGenerate capabilities（needs update）

  **Acceptance Criteria**:
  - [ ] Gemini thinking Part round-trip 正确
  - [ ] thoughts_token_count 传递到 IR
  - [ ] WireCapabilities 更新为 supports_thinking: true

  **QA Scenarios**:

  ```
  Scenario: Gemini thinking Part parse
    Tool: Bash
    Steps:
      1. fixture: Gemini response 含 {"text":"thinking step","thought":true}
      2. parse → ResponseIR.thinking 包含 ThinkingBlockIR
    Expected Result: thinking 内容正确映射
    Evidence: .omo/evidence/task-9-gemini-thinking.txt
  ```

  **Commit**: YES (groups with Wave 3)
  - Files: `crates/gw-codec/src/gemini/wire.rs`, `crates/gw-codec/src/gemini/via.rs`, `crates/gw-codec/src/cross_render.rs`

- [x] 10. OpenAI Responses output item 扩展 (Gap #11)

  **What to do**:
  - 在 `crates/gw-codec/src/openai/responses/wire_response.rs` 中扩展 output item types：
    - 当前只有 Message, Reasoning, FunctionCall
    - 新增：`WebSearchCall`, `FileSearchCall`, `ComputerCall`, `ComputerCallOutput`, `CodeInterpreterCall`, `ImageGenerationCall`, `CustomToolCall`, `CustomToolCallOutput`, `McpCall`, `McpListTools`, `ShellCall`, `ShellCallOutput`, `ApplyPatchCall`, `ApplyPatchCallOutput`, `ToolSearchCall`, `ToolSearchOutput`
    - 每个新 item type 使用 typed struct，关键字段强类型化，其余 `#[serde(flatten)] extra: serde_json::Map`
    - 添加 `Compaction` item type（可 opaque）
  - 在 wire_response.rs Message content 中新增 `Refusal { refusal: Cow<'a, str> }` 变体
  - 在 via.rs parse/render 中映射新 item types → IR：
    - `WebSearchCall`, `FileSearchCall`, `ComputerCall`, `CodeInterpreterCall`, `ImageGenerationCall`, `McpCall`, `ShellCall`, `ApplyPatchCall`, `ToolSearchCall` → `ResponseIR.server_tool_blocks` (同 Anthropic server tool block 的 IR 路径，复用 Task 1 新增的 `ServerToolBlockIR` 类型)
    - `FunctionCall`/`FunctionCallOutput` → 已有的 tool_calls IR 路径（保持现有行为）
    - `Reasoning` → 已有的 thinking/reasoning IR 路径（保持现有行为）
    - `Compaction`, `ComputerCallOutput`, `CustomToolCall/Output`, `ShellCallOutput`, `ApplyPatchCallOutput`, `McpListTools`, `ToolSearchOutput` → `ServerToolBlockIR` with opaque inner（这些是 Responses API 独有的 lifecycle item，无跨协议语义）
    - 跨协议 render: 非 Responses 目标时 `ServerToolBlockIR` 走 `adapt_lossy` drop（如 Anthropic/Gemini 目标）
  - 在 `wire_stream_chunk.rs` 中确保新 item types 的 stream events 不 panic（可以 skip/log）

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 大量 output item types，需要精确 serde tagged enum 设计
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 22
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-codec/src/openai/responses/wire_response.rs` — 现有 output item enum
  - `crates/gw-codec/src/openai/responses/via.rs` — parse/render
  - `crates/gw-codec/src/openai/responses/wire_stream_chunk.rs` — stream events

  **Acceptance Criteria**:
  - [ ] 24 种 output item type 可 parse（不 panic）
  - [ ] Message + FunctionCall + Reasoning 保持现有行为
  - [ ] 新 item types 在 non-Responses 目标走 lossy drop

  **QA Scenarios**:

  ```
  Scenario: web_search_call output item parse
    Tool: Bash
    Steps:
      1. fixture: {"type":"web_search_call","id":"ws_01","status":"completed"}
      2. parse → 不 panic，正确映射
    Expected Result: 成功 parse 为 WebSearchCall variant
    Evidence: .omo/evidence/task-10-web-search-item.txt
  ```

  **Commit**: YES (groups with Wave 3)
  - Files: `crates/gw-codec/src/openai/responses/wire_response.rs`, `crates/gw-codec/src/openai/responses/via.rs`

- [x] 11. Anthropic document + search_result input blocks (Gap #12)

  **What to do**:
  - 在 `crates/gw-codec/src/anthropic/wire.rs` 的请求侧 content block 中新增：
    - `Document { source: DocumentSource<'a>, title: Option<Cow<'a, str>>, context: Option<Cow<'a, str>>, cache_control: Option<...> }`
    - `SearchResult { source: Cow<'a, str>, title: Cow<'a, str>, content: Cow<'a, str>, cache_control: Option<...> }`
    - `DocumentSource` enum: `Base64 { media_type, data }`, `Text { text }`, `Url { url }`, `Content { content: Vec<...> }`
  - 在 via.rs parse_request 中：这些 block types 映射到 IR `ContentPart`（Document → File, SearchResult → Text with metadata）
  - 在 via.rs render_request 中：从 IR 反向映射

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-codec/src/anthropic/wire.rs:67-103` — 现有 request content blocks

  **Acceptance Criteria**:
  - [ ] Document block parse 支持 base64/text/url source
  - [ ] SearchResult block parse 正确

  **QA Scenarios**:

  ```
  Scenario: Document block with URL source
    Tool: Bash
    Steps:
      1. fixture: {"type":"document","source":{"type":"url","url":"https://example.com/doc.pdf"},"title":"Test"}
      2. parse → ContentPart::File
    Expected Result: 正确映射
    Evidence: .omo/evidence/task-11-document-block.txt
  ```

  **Commit**: YES (groups with Wave 3)
  - Files: `crates/gw-codec/src/anthropic/wire.rs`, `crates/gw-codec/src/anthropic/via.rs`

- [x] 12. OpenAI reasoning_effort + service_tier (Gap #14, #21)

  **What to do**:
  - 在 `crates/gw-codec/src/openai/wire.rs` `OpenAiRequest` 中新增：
    - `reasoning_effort: Option<Cow<'a, str>>` — none|minimal|low|medium|high|xhigh
    - `service_tier: Option<Cow<'a, str>>` — auto|default|flex|scale|priority
  - 在 Response wire 中新增 `service_tier: Option<Cow<'a, str>>`
  - 在 via.rs parse/render 中：
    - `reasoning_effort` → `RequestIR.reasoning_config.effort`
    - `service_tier` → `RequestIR.service_tier` / `ResponseIR.service_tier`
  - 在 Responses API wire 中同步：`reasoning.effort` → 同一 IR 字段

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 16
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-codec/src/openai/wire.rs:11-51` — OpenAiRequest struct

  **Acceptance Criteria**:
  - [ ] reasoning_effort round-trip 正确
  - [ ] service_tier 在 request + response 中 round-trip

  **QA Scenarios**:

  ```
  Scenario: reasoning_effort round-trip
    Tool: Bash
    Steps:
      1. request fixture: {"reasoning_effort":"high",...}
      2. parse → IR.reasoning_config.effort == Some("high")
      3. render → 输出含 reasoning_effort: "high"
    Expected Result: 完整 round-trip
    Evidence: .omo/evidence/task-12-reasoning-effort.txt
  ```

  **Commit**: YES (groups with Wave 3)
  - Files: `crates/gw-codec/src/openai/wire.rs`, `crates/gw-codec/src/openai/via.rs`, `crates/gw-codec/src/openai/responses/wire_request.rs`, `crates/gw-codec/src/openai/responses/via.rs`

- [x] 13. OpenAI audio modality wire (Gap #16)

  **What to do**:
  - 在 `crates/gw-codec/src/openai/wire.rs` `OpenAiRequest` 中新增：
    - `modalities: Option<Vec<Cow<'a, str>>>` — ["text", "audio"]
    - `audio: Option<OpenAiAudioConfig<'a>>` — 新 struct `{ voice: Cow, format: Cow }`
  - 在 Response message 中新增 `audio: Option<OpenAiAudioResponse<'a>>` — `{ id, data, expires_at, transcript }`
  - 在 via.rs parse/render 中映射到 IR `AudioConfig` / response audio
  - IR `ContentPart::Audio` 已存在——确保 OpenAI wire input_audio → IR Audio → OpenAI wire input_audio round-trip

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-core/src/ir.rs:90` — `ContentPart::Audio{source: MediaSource}` 已存在
  - `crates/gw-codec/src/openai/wire.rs:11-51` — OpenAiRequest

  **Acceptance Criteria**:
  - [ ] audio config request round-trip
  - [ ] audio response output parse 正确

  **QA Scenarios**:

  ```
  Scenario: Audio config round-trip
    Tool: Bash
    Steps:
      1. fixture: {"modalities":["text","audio"],"audio":{"voice":"alloy","format":"wav"},...}
      2. parse → IR.audio_config
      3. render → 输出含 modalities + audio
    Expected Result: round-trip 正确
    Evidence: .omo/evidence/task-13-audio-config.txt
  ```

  **Commit**: YES (groups with Wave 3)
  - Files: `crates/gw-codec/src/openai/wire.rs`, `crates/gw-codec/src/openai/via.rs`

- [x] 14. Anthropic thinking config 新选项 (Gap #15)

  **What to do**:
  - 在 `crates/gw-codec/src/anthropic/wire.rs` 请求 thinking 配置中扩展：
    - 支持 `display: Option<Cow<'a, str>>` — "summarized" | "omitted"
    - 支持 adaptive thinking: `{type: "adaptive", display: Option<Cow>}`
    - 支持 `output_config.effort` — low|medium|high|xhigh|max
  - 在 via.rs parse_request 中映射到 IR `AnthropicThinking` 扩展字段 + `ReasoningConfig`
  - 在 via.rs render_request 中反向映射

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 现有 thinking 结构的字段扩展
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 23
  - **Blocked By**: Tasks 1, 5

  **References**:
  - `crates/gw-core/src/ir.rs:155-158` — AnthropicThinking struct（Task 1 扩展后）
  - `crates/gw-codec/src/anthropic/wire.rs:17-43` — AnthropicRequest

  **Acceptance Criteria**:
  - [ ] display/adaptive thinking config round-trip
  - [ ] output_config.effort 映射到 IR ReasoningConfig

  **QA Scenarios**:

  ```
  Scenario: Adaptive thinking config
    Tool: Bash
    Steps:
      1. fixture: {"thinking":{"type":"adaptive","display":"summarized"}}
      2. parse → IR anthropic_thinking
    Expected Result: adaptive + display 保留
    Evidence: .omo/evidence/task-14-adaptive-thinking.txt
  ```

  **Commit**: YES (groups with Wave 3)
  - Files: `crates/gw-codec/src/anthropic/wire.rs`, `crates/gw-codec/src/anthropic/via.rs`

- [x] 15. OpenAI structured output 确认 + response_format 完善 (Gap #13)

  **What to do**:
  - 确认 `ResponseFormatIR::JsonSchema(Box<RawValue>)` 是否完整支持 `json_schema{name,description,schema,strict}` 子结构
  - 如果 OpenAI wire 中 `response_format` 只是 opaque RawValue 透传则已 OK
  - 如果需要强类型化：新增 `OpenAiResponseFormat` enum: `Text`, `JsonObject`, `JsonSchema { name, description, schema, strict }`
  - 确保 Anthropic `output_config.format.json_schema` 也映射到同一 IR 字段
  - 确保 Gemini `response_schema` + `response_mime_type` 映射正确

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 可能只需确认已有实现足够
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-core/src/ir.rs:135-139` — ResponseFormatIR enum

  **Acceptance Criteria**:
  - [ ] json_schema response_format 在三家协议间 round-trip

  **QA Scenarios**:

  ```
  Scenario: JSON Schema response_format cross-protocol
    Tool: Bash
    Steps:
      1. OpenAI request: {"response_format":{"type":"json_schema","json_schema":{"name":"person","schema":{"type":"object"}}}}
      2. parse → IR → render as Gemini (response_mime_type + response_schema)
    Expected Result: schema 内容保留
    Evidence: .omo/evidence/task-15-json-schema.txt
  ```

  **Commit**: YES (groups with Wave 3)
  - Files: depending on findings

- [x] 16. adapt_lossy 全矩阵 + finish_reason 扩展 (Gap #17, #28)

  **What to do**:
  - 在 `crates/gw-codec/src/cross_render.rs` `adapt_lossy()` 中为所有新 IR 字段添加完整的 4×4 lossy 逻辑：
    - refusal: drop for Gemini (no refusal concept)
    - annotations: drop for Anthropic/Gemini
    - server_tool_blocks: drop for OpenAI/Gemini
    - redacted_thinking: drop for OpenAI Chat/Gemini
    - audio: drop for Anthropic/Gemini
    - service_tier: keep for OpenAI/Anthropic, drop for Gemini
    - reasoning_config.effort: 跨协议映射（OpenAI effort string → Anthropic budget_tokens 近似转换; → Gemini thinkingBudget）
    - cached_tokens: OpenAI → Anthropic 映射为 cache_read_input_tokens
  - 在 `map_finish_reason()` 中扩展 Gemini 特有 finish reasons：
    - `BLOCKLIST`, `PROHIBITED_CONTENT`, `SPII` → `ContentFilter`
    - `MALFORMED_FUNCTION_CALL` → `Other("malformed_function_call")`
    - `MODEL_ARMOR` → `ContentFilter`
    - 其余未知值 → `Other(s)`
  - 添加全矩阵测试：每个新字段 × 4 from × 4 to = 所有 drop/keep 决策

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 4×4 矩阵设计需要精确理解每个协议的语义映射
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 23
  - **Blocked By**: Tasks 2, 8, 9, 12

  **References**:
  - `crates/gw-codec/src/cross_render.rs:144-198` — 现有 adapt_lossy + map_finish_reason
  - `crates/gw-codec/src/cross_render.rs:275-368` — 现有 matrix test pattern

  **Acceptance Criteria**:
  - [ ] 每个新 IR 字段在 adapt_lossy 中有 drop/keep 逻辑
  - [ ] 每个 lossy drop 调用 LossyField::drop()
  - [ ] Gemini 新 finish reasons 正确映射
  - [ ] 全矩阵测试覆盖

  **QA Scenarios**:

  ```
  Scenario: adapt_lossy 全矩阵
    Tool: Bash
    Steps:
      1. 构造含所有新字段的 ResponseIR
      2. 对 4×4 组合 run adapt_lossy
      3. assert 每个组合的 drop/keep 决策正确
    Expected Result: 全 PASS
    Evidence: .omo/evidence/task-16-lossy-matrix.txt
  ```

  **Commit**: YES (groups with Wave 4)
  - Files: `crates/gw-codec/src/cross_render.rs`

- [x] 17. Anthropic cache TTL + inference_geo + service_tier (Gap #18, #26)

  **What to do**:
  - 在 `crates/gw-codec/src/anthropic/wire.rs` 中：
    - `AnthropicRequest` 新增 `cache_control: Option<AnthropicCacheControl<'a>>` — `{ kind: "ephemeral", ttl: Option<Cow> }`
    - `service_tier: Option<Cow<'a, str>>`
    - `inference_geo: Option<Cow<'a, str>>`
  - 在 `AnthropicUsage` 中新增：
    - `cache_creation: Option<AnthropicCacheCreationDetails>` — `{ ephemeral_1h_input_tokens: Option<u32>, ephemeral_5m_input_tokens: Option<u32> }`
    - `inference_geo: Option<Cow<'a, str>>`
    - `server_tool_use: Option<AnthropicServerToolUsage>` — `{ web_search_requests: Option<u32>, web_fetch_requests: Option<u32> }`
    - `service_tier: Option<Cow<'a, str>>`
  - 在 via.rs 中映射到 IR UsageIR 对应字段

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-codec/src/anthropic/wire.rs:126-148` — AnthropicUsage struct

  **Acceptance Criteria**:
  - [ ] cache TTL 5m/1h 分级 usage round-trip
  - [ ] inference_geo + service_tier round-trip

  **QA Scenarios**:

  ```
  Scenario: Cache creation TTL usage
    Tool: Bash
    Steps:
      1. response fixture: usage.cache_creation.ephemeral_1h_input_tokens = 500
      2. parse → IR.usage.cache_creation_1h_tokens == Some(500)
    Expected Result: 正确映射
    Evidence: .omo/evidence/task-17-cache-ttl.txt
  ```

  **Commit**: YES (groups with Wave 4)
  - Files: `crates/gw-codec/src/anthropic/wire.rs`, `crates/gw-codec/src/anthropic/via.rs`, `crates/gw-codec/src/anthropic/usage_extractor.rs`

- [x] 18. Gemini executableCode + codeExecutionResult + grounding (Gap #19, #20)

  **What to do**:
  - 在 `crates/gw-codec/src/gemini/wire.rs` `GeminiPart` 中新增变体：
    - `ExecutableCode { code: Cow<'a, str>, language: Option<Cow<'a, str>> }`
    - `CodeExecutionResult { outcome: Cow<'a, str>, output: Option<Cow<'a, str>> }`
  - 在 `GeminiCandidate` 中新增 `grounding_metadata: Option<Box<RawValue>>` — grounding 结构复杂，使用 opaque RawValue 透传
  - 在 via.rs 中：
    - parse: `ExecutableCode` → `ServerToolBlockIR { kind: "executable_code", ... }`; `CodeExecutionResult` → `ServerToolBlockIR { kind: "code_execution_result", ... }`（复用 Task 1 的 ServerToolBlockIR，与 Anthropic code_execution_tool_result 共享 IR 路径）
    - `grounding_metadata`: 存入 `ResponseIR.metadata.extensions` (类型 `serde_json::Map`)。理由：grounding_metadata 是 Gemini 独有的搜索引用数据，无跨协议对等概念（OpenAI 的 annotations 和 Anthropic 的 citations 有各自独立字段），因此作为 vendor-specific metadata 存储而非 IR 一级字段。跨协议 render 时由 adapt_lossy drop + 计 lossy counter。

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-codec/src/gemini/wire.rs:61-95` — GeminiPart enum
  - `crates/gw-codec/src/gemini/wire.rs:171-185` — GeminiCandidate

  **Acceptance Criteria**:
  - [ ] ExecutableCode/CodeExecutionResult Part round-trip
  - [ ] grounding_metadata 保留（opaque）

  **QA Scenarios**:

  ```
  Scenario: ExecutableCode Part parse
    Tool: Bash
    Steps:
      1. fixture: {"executableCode":{"code":"print('hello')","language":"PYTHON"}}
      2. parse → GeminiPart::ExecutableCode
    Expected Result: 正确 parse
    Evidence: .omo/evidence/task-18-executable-code.txt
  ```

  **Commit**: YES (groups with Wave 4)
  - Files: `crates/gw-codec/src/gemini/wire.rs`, `crates/gw-codec/src/gemini/via.rs`

- [x] 19. OpenAI web_search + prediction + custom tool (Gap #22, #23, #24)

  **What to do**:
  - 在 `crates/gw-codec/src/openai/wire.rs` `OpenAiRequest` 中新增：
    - `web_search_options: Option<Box<RawValue>>` — opaque 透传
    - `prediction: Option<Box<RawValue>>` — opaque 透传
  - 在 `OpenAiTool` 中扩展支持 `custom` type（除 function 外）：
    - 新增 `custom: Option<OpenAiCustomTool<'a>>` — `{ name, description, format }` or 使用 untagged enum
  - 在 via.rs 中映射到 IR 对应字段（web_search, prediction 透传; custom tool → ToolIR with metadata）

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 23
  - **Blocked By**: Task 1

  **References**:
  - `crates/gw-codec/src/openai/wire.rs:11-51` — OpenAiRequest
  - `crates/gw-codec/src/openai/wire.rs:102-118` — OpenAiTool

  **Acceptance Criteria**:
  - [ ] web_search_options 透传 round-trip
  - [ ] prediction 透传 round-trip
  - [ ] custom tool type parse 不 panic

  **QA Scenarios**:

  ```
  Scenario: web_search_options round-trip
    Tool: Bash
    Steps:
      1. fixture: {"web_search_options":{"search_context_size":"medium"},...}
      2. parse → IR → render → 输出含 web_search_options
    Expected Result: opaque 透传完整
    Evidence: .omo/evidence/task-19-web-search.txt
  ```

  **Commit**: YES (groups with Wave 4)
  - Files: `crates/gw-codec/src/openai/wire.rs`, `crates/gw-codec/src/openai/via.rs`

- [x] 20. Usage 细分字段全协议扩展 (Gap #27)

  **What to do**:
  - 在 `crates/gw-codec/src/openai/wire.rs` `OpenAiUsage` / `OpenAiCompletionTokensDetails` / `OpenAiPromptTokensDetails` 中新增：
    - `audio_tokens: Option<u32>` (in both prompt + completion details)
    - `accepted_prediction_tokens: Option<u32>`, `rejected_prediction_tokens: Option<u32>` (completion details)
  - 在 `crates/gw-codec/src/gemini/wire.rs` `GeminiUsage` 中新增：
    - `tool_use_prompt_token_count: Option<u32>`
  - 在各 codec 的 `usage_extractor.rs` 中映射新字段到 IR UsageIR 对应字段
  - 确保 adapt_lossy 中新 usage 字段按目标协议能力 drop

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 23
  - **Blocked By**: Tasks 1, 2

  **References**:
  - `crates/gw-codec/src/openai/wire.rs:152-175` — OpenAiUsage + details
  - `crates/gw-codec/src/gemini/wire.rs:187-198` — GeminiUsage
  - `crates/gw-codec/src/openai/usage_extractor.rs` — usage 提取 pattern
  - `crates/gw-codec/src/anthropic/usage_extractor.rs` — Anthropic usage
  - `crates/gw-codec/src/gemini/usage_extractor.rs` — Gemini usage

  **Acceptance Criteria**:
  - [ ] OpenAI audio_tokens round-trip
  - [ ] Gemini tool_use_prompt_token_count round-trip
  - [ ] 跨协议 usage 细分字段按能力 lossy drop

  **QA Scenarios**:

  ```
  Scenario: OpenAI usage details round-trip
    Tool: Bash
    Steps:
      1. response fixture: usage.completion_tokens_details.audio_tokens=50
      2. parse → IR.usage.audio_tokens_out == Some(50)
    Expected Result: 映射正确
    Evidence: .omo/evidence/task-20-usage-details.txt
  ```

  **Commit**: YES (groups with Wave 4)
  - Files: `crates/gw-codec/src/openai/wire.rs`, `crates/gw-codec/src/gemini/wire.rs`, `crates/gw-codec/src/*/usage_extractor.rs`

- [x] 21. Anthropic citations_delta + MCP connector blocks (Gap #25, #30)

  **What to do**:
  - 在 `crates/gw-codec/src/anthropic/wire.rs` stream delta 中新增：
    - `CitationsDelta { data: Cow<'a, str> }` 变体 in `AnthropicStreamDelta`
  - 在 `AnthropicContent` 请求侧工具列表中新增 `McpToolset` type（`mcp_tool_use`, `mcp_tool_result` block types）
  - 在 via.rs stream parse 中：`citations_delta` → `StreamEventIR::CitationsDelta`
  - 在 via.rs 中：MCP blocks → IR ServerToolBlockIR

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 23
  - **Blocked By**: Tasks 1, 5

  **References**:
  - `crates/gw-codec/src/anthropic/wire.rs:193-220` — AnthropicStreamDelta
  - `crates/gw-codec/src/anthropic/wire.rs:67-103` — AnthropicContent

  **Acceptance Criteria**:
  - [ ] citations_delta stream event parse
  - [ ] MCP tool blocks parse 不 panic

  **QA Scenarios**:

  ```
  Scenario: citations_delta parse
    Tool: Bash
    Steps:
      1. SSE fixture: event:content_block_delta, data:{"type":"citations_delta","citation":{"...":"..."}}
      2. parse → StreamEventIR::CitationsDelta
    Expected Result: 正确 parse
    Evidence: .omo/evidence/task-21-citations-delta.txt
  ```

  **Commit**: YES (groups with Wave 4)
  - Files: `crates/gw-codec/src/anthropic/wire.rs`, `crates/gw-codec/src/anthropic/via.rs`, `crates/gw-codec/src/anthropic/codec.rs`

- [x] 22. OpenAI Responses streaming 50+ event types (Gap #29)

  **What to do**:
  - 在 `crates/gw-codec/src/openai/responses/wire_stream_chunk.rs` 中扩展 stream event enum 覆盖所有 50+ event types：
    - 关键新事件：`response.refusal.delta`, `response.refusal.done`, `response.reasoning_summary_text.delta`, `response.reasoning_text.delta`, `response.file_search_call.*`, `response.web_search_call.*`, `response.code_interpreter_call.*`, `response.image_generation_call.*`, `response.mcp_call.*`, `response.audio.*`
    - 非关键事件（lifecycle）：`response.created`, `response.queued`, `response.cancelled` 等 → 可以 skip 但不能 panic
  - 在 codec.rs stream parse 中：已知事件 → 对应 StreamEventIR; 未知事件 → log + skip
  - 关键映射：
    - `response.refusal.delta` → `StreamEventIR::RefusalDelta`
    - `response.reasoning_summary_text.delta` → `StreamEventIR::ThinkingDelta`（近似映射）
    - `response.text.delta` → `StreamEventIR::Delta{text}`（已有）
    - 工具类事件 → `StreamEventIR::ServerToolStart/Delta`

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 大量 event type 的精确映射 + graceful unknown 处理
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: Task 23
  - **Blocked By**: Task 10

  **References**:
  - `crates/gw-codec/src/openai/responses/wire_stream_chunk.rs` — 现有 stream chunk 类型
  - `crates/gw-codec/src/openai/responses/codec.rs` — stream parse 逻辑

  **Acceptance Criteria**:
  - [ ] 已知事件正确映射
  - [ ] 未知事件不 panic，graceful skip
  - [ ] refusal + reasoning stream events 传递到 IR

  **QA Scenarios**:

  ```
  Scenario: response.refusal.delta stream
    Tool: Bash
    Steps:
      1. SSE fixture: event:response.refusal.delta, data:{"delta":"I cannot"}
      2. parse → StreamEventIR::RefusalDelta{text:"I cannot"}
    Expected Result: 正确映射
    Evidence: .omo/evidence/task-22-refusal-stream.txt

  Scenario: Unknown event graceful skip
    Tool: Bash
    Steps:
      1. SSE fixture: event:response.future_unknown_event, data:{...}
      2. parse → 不 panic，skip
    Expected Result: 无 panic，日志记录
    Evidence: .omo/evidence/task-22-unknown-event.txt
  ```

  **Commit**: YES (groups with Wave 4)
  - Files: `crates/gw-codec/src/openai/responses/wire_stream_chunk.rs`, `crates/gw-codec/src/openai/responses/codec.rs`

- [x] 23. 全量 round-trip 测试 + integration

  **What to do**:
  - 为所有 30 个 gap 编写 round-trip 测试：
    - 每个 gap 至少 1 个 happy-path parse→IR→render→re-parse 测试
    - 每个 gap 至少 1 个 cross-render 测试（从源协议到非支持目标，验证 lossy drop）
  - 测试组织：
    - `crates/gw-codec/src/openai/` — OpenAI Chat + Responses 的 gap 测试
    - `crates/gw-codec/src/anthropic/` — Anthropic 的 gap 测试
    - `crates/gw-codec/src/gemini/` — Gemini 的 gap 测试
    - `crates/gw-codec/src/cross_render.rs` — 跨协议矩阵测试
  - 创建 test fixture JSON 文件（可 inline 或在 tests/ 目录）
  - 运行全量 `cargo test --workspace` 确认无 regression

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: 需要为 30 个 gap 设计全面测试用例
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Task 24)
  - **Parallel Group**: Wave 5
  - **Blocks**: Task 25
  - **Blocked By**: All Tasks 1-22

  **References**:
  - `crates/gw-codec/src/cross_render.rs:200-368` — 现有测试 pattern
  - 各 codec 目录下现有测试文件

  **Acceptance Criteria**:
  - [ ] `cargo test --workspace` 全 PASS
  - [ ] 每个 gap 至少 1 个 round-trip 测试
  - [ ] 跨协议 lossy 矩阵测试覆盖

  **QA Scenarios**:

  ```
  Scenario: 全量测试通过
    Tool: Bash
    Steps:
      1. cargo test --workspace 2>&1
      2. 检查输出 test result: ok. X passed; 0 failed
    Expected Result: 0 failed
    Evidence: .omo/evidence/task-23-full-test.txt
  ```

  **Commit**: YES (Wave 5)
  - Message: `test(codec): add round-trip tests for all 30 protocol gaps`
  - Pre-commit: `cargo test --workspace`

- [x] 24. docs/architecture.md + docs/go-contracts/ 同步更新

  **What to do**:
  - 更新 `docs/architecture.md`：
    - §Public traits: 如有新增 trait 需记录
    - §Persistence namespaces: 如 usage:detailed 形状变化需记录
    - §Deliberate behaviours: 记录新的 lossy drop 行为和 opaque inner 决策
  - 更新 `docs/go-contracts/`：
    - 如果 `usage:detailed` archive 的 JSON payload 因新 IR 字段而变化（新字段出现在序列化输出中），更新对应 parts/ 文件
    - 记录新 UsageIR 字段在 archive payload 中的位置
  - 在 AGENTS.md 描述的 "Capability surface highlights" 中更新 Wire formats 描述（如果有新 block type 被模型支持）

  **Recommended Agent Profile**:
  - **Category**: `writing`
    - Reason: 文档同步更新
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: YES (with Task 23)
  - **Parallel Group**: Wave 5
  - **Blocks**: -
  - **Blocked By**: All Tasks 1-22

  **References**:
  - `docs/architecture.md` — 需更新的目标文档
  - `docs/go-contracts/main.md` + `docs/go-contracts/parts/` — Go contract 文档

  **Acceptance Criteria**:
  - [ ] docs 更新覆盖所有新 IR 类型
  - [ ] go-contracts 更新如 usage:detailed 变化

  **QA Scenarios**:

  ```
  Scenario: Sync rule 验证
    Tool: Bash
    Steps:
      1. git diff --name-only 检查变更文件
      2. 验证 ir.rs 变更时 architecture.md 也变更
      3. 验证 usage 字段变更时 go-contracts/ 也变更
    Expected Result: sync rule 满足
    Evidence: .omo/evidence/task-24-sync-rule.txt
  ```

  **Commit**: YES (Wave 5)
  - Message: `docs: sync architecture and go-contracts for codec gap coverage`
  - Files: `docs/architecture.md`, `docs/go-contracts/`

- [x] 25. make e2e-soak-smoke 验证 (**BLOCKED**: Docker daemon not running on dev machine; per AGENTS.md this is "recommended as a final local sanity check, not a CI gate")

  **What to do**:
  - 运行 `make e2e-soak-smoke`（100 rps × 10s）验证无 regression
  - 检查：
    - 进程存活
    - 账户一致性
    - 资源 gauge 释放
    - 无新 error code
  - 如果发现 regression，报告并协调修复

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: 只需运行命令 + 检查输出
  - **Skills**: []

  **Parallelization**:
  - **Can Run In Parallel**: NO
  - **Parallel Group**: Sequential (after Task 23)
  - **Blocks**: F1-F4
  - **Blocked By**: Task 23

  **References**:
  - AGENTS.md §Common commands — soak smoke 说明

  **Acceptance Criteria**:
  - [ ] `make e2e-soak-smoke` 退出 0
  - [ ] 无 resource leak
  - [ ] 无新 error code

  **QA Scenarios**:

  ```
  Scenario: Soak smoke 通过
    Tool: Bash
    Steps:
      1. make e2e-soak-smoke 2>&1
      2. 检查退出码 == 0
      3. 检查 .soak_report/ 无 FAIL
    Expected Result: exit 0, no failures
    Evidence: .omo/evidence/task-25-soak-smoke.txt
  ```

  **Commit**: NO (verification only)

---

## Final Verification Wave

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.

- [x] F1. **Plan Compliance Audit** — `oracle`
  Read the plan end-to-end. For each "Must Have": verify implementation exists (read file, run `cargo test`). For each "Must NOT Have": search codebase for forbidden patterns — reject with file:line if found. Check evidence files exist in .omo/evidence/. Compare deliverables against plan.

  **Acceptance Criteria**:
  - [ ] Must Have 列表 9/9 项全部验证存在
  - [ ] Must NOT Have 列表 11 项全部 grep 确认不存在
  - [ ] .omo/evidence/ 下存在所有 task 对应的 evidence 文件
  - [ ] Output format: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [N/N] | VERDICT: APPROVE/REJECT`

- [x] F2. **Code Quality Review** — `unspecified-high`
  Run `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all -- --check` + `cargo test --workspace`. Review all changed files for: `unsafe`, `#[allow(clippy::*)]` without justification, empty match arms, `dbg!()` in prod, commented-out code, unused imports. Check AI slop: excessive comments, over-abstraction, generic names.

  **Acceptance Criteria**:
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings` 退出 0
  - [ ] `cargo fmt --all -- --check` 退出 0
  - [ ] `cargo test --workspace` 全 PASS
  - [ ] 无 `dbg!()` / commented-out code / unused imports in changed files
  - [ ] Output format: `Build [PASS/FAIL] | Lint [PASS/FAIL] | Tests [N pass/N fail] | Files [N clean/N issues] | VERDICT`

- [x] F3. **Real Manual QA** — `unspecified-high`
  Start from clean state. Execute EVERY QA scenario from EVERY task — follow exact steps, capture evidence. Test cross-task integration (multimodal + cross-render, thinking + stream, etc.). Test edge cases: empty content parts, null fields, unknown future fields. Save to `.omo/evidence/final-qa/`.

  **Acceptance Criteria**:
  - [ ] 每个 task 的每个 QA scenario 执行并生成 evidence 文件
  - [ ] 至少 3 个跨 task integration 测试（如 multimodal→cross-render, thinking→stream, server-tool→lossy）
  - [ ] 至少 5 个边界 case 测试（empty parts, null fields, unknown future fields, 0-token usage, mixed modality）
  - [ ] Output format: `Scenarios [N/N pass] | Integration [N/N] | Edge Cases [N tested] | VERDICT`

- [x] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual diff. Verify 1:1 — everything in spec was built (no missing), nothing beyond spec was built (no creep). Check "Must NOT do" compliance. Detect cross-task contamination. Flag unaccounted changes.

  **Acceptance Criteria**:
  - [ ] 25 个 task 的 "What to do" 与实际 diff 1:1 对应
  - [ ] 无 scope creep（diff 中无 plan 未提及的变更）
  - [ ] 无 cross-task contamination（Task N 未改 Task M 的文件）
  - [ ] Output format: `Tasks [N/N compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

每个 Wave 完成后做一次原子提交：

- **Wave 1**: `feat(codec): extend IR types and WireCapabilities for protocol gap coverage` — ir.rs, cross_render.rs
- **Wave 2**: `feat(codec): fix P0 protocol gaps — multimodal, developer role, server tools, gen config` — openai/wire.rs, anthropic/wire.rs, gemini/wire.rs, via.rs files
- **Wave 3**: `feat(codec): fix P1 protocol gaps — thinking, responses items, audio, reasoning` — multiple codec files
- **Wave 4**: `feat(codec): fix P2 protocol gaps — lossy matrix, usage details, streaming events` — cross_render.rs, wire files
- **Wave 5**: `test(codec): add round-trip tests + docs sync` — test files, docs/

Pre-commit for all: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && bash scripts/check-invariants.sh`

---

## Success Criteria

### Verification Commands
```bash
cargo test --workspace                                    # Expected: all pass
cargo clippy --workspace --all-targets -- -D warnings     # Expected: 0 warnings
cargo fmt --all -- --check                                # Expected: 0 diffs
bash scripts/check-invariants.sh                          # Expected: exit 0
make e2e-soak-smoke                                       # Expected: exit 0
! grep -r 'todo!()\|unimplemented!()\|TODO(v2)' crates/   # Expected: no matches
```

### Final Checklist
- [ ] All 30 gaps addressed (P0: 4, P1: 12, P2: 14)
- [ ] All "Must Have" present
- [ ] All "Must NOT Have" absent
- [ ] All tests pass
- [ ] adapt_lossy covers all new fields with lossy counters
- [ ] WireCapabilities extended for all new capabilities
- [ ] docs synced per AGENTS.md sync rule
