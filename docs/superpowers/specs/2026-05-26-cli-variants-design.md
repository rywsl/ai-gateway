# CLI Protocol Variants — 设计文档（v2026-05-26）

- **日期**：2026-05-26
- **状态**：Draft（待审阅）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-codec-s1-ir-and-codegen.md](./2026-05-19-codec-s1-ir-and-codegen.md)（IR schema + wire 直转）
  - [2026-05-19-adapter-a-t0-trait-v2.md](./2026-05-19-adapter-a-t0-trait-v2.md)（UpstreamAdapter trait）
  - [2026-05-19-adapter-a-t1-claude-code.md](./2026-05-19-adapter-a-t1-claude-code.md)（ClaudeCode adapter）
  - [2026-05-19-adapter-a-t2-gemini-cli.md](./2026-05-19-adapter-a-t2-gemini-cli.md)（GeminiCli adapter）
  - [2026-05-24-v2-codec-cleanup-design.md](./2026-05-24-v2-codec-cleanup-design.md)（S-codec v2）
  - Aether 来源：`src/api/handlers/base/cli_adapter_base.py`、`src/api/handlers/{claude_cli,openai_cli,gemini_cli}/`
- **范围**：CLI 协议变体在 Rust 网关中的完整处理：FORMAT_ID 注册、请求检测、透明代理模型、filter chain 保留、无 IR 计费、CliCodec trait、流式支持、跨协议约束。
- **不在范围**：
  - 具体 vendor adapter 实现（→ A-T1 ~ A-T2 已有）；
  - Go 侧计费公式（`no_cost_micro` 约束）；
  - task worker 长任务路径（→ task-worker-design.md）；
  - UI 管理面。

> CLI 变体是 Aether 最活跃的流量入口之一（Claude Code、Gemini CLI、OpenAI Codex/CLI）。它们与标准 API 的核心差异是**请求/响应体透传不过 IR**，但 filter chain（鉴权、权限、限速、计费）完全保留。本 spec 是 Rust 网关 CLI 支持的第一份规范文档。

---

## 1. 背景与问题

### 1.1 CLI 工具的流量特征

| CLI 工具 | 上游协议 | 入口路径 | 鉴权 | 特殊性 |
|----------|---------|---------|------|--------|
| Claude Code | Anthropic Messages | `/v1/messages` | OAuth Bearer（无 x-api-key） | anthropic-beta headers、Stainless 指纹 |
| OpenAI Codex / CLI | OpenAI Responses | `/v1/responses` | Bearer | `response.completed` 用量事件 |
| OpenAI Compact | OpenAI Responses | `/v1/responses/compact` | Bearer | 紧凑格式变体 |
| Gemini CLI | Gemini v1internal | `/v1beta/models/{model}:{action}` | API key（query/header） | v1internal URL、`x-app: cli`、`alt=sse` |

### 1.2 与标准 API 的关键差异

标准 API 路径（OpenAI Chat `/v1/chat/completions`、Anthropic `/v1/messages`、Gemini `/v1/models/...`）走完整的 codec 通路：

```
client body → parse_request → RequestIR → render_request → upstream body
upstream body → parse_response → ResponseIR → render_response → client body
```

CLI 变体不走 IR 转换——**请求/响应体是透传的**。理由：

1. CLI 工具对 wire 格式敏感（vendor 专有字段、扩展参数、thinking signatures）；IR 无法无损表达所有 vendor 特有字段。
2. CLI 流量几乎总是同协议——Claude Code 到 Anthropic、Codex 到 OpenAI、Gemini CLI 到 Gemini，不需要跨协议转换。
3. 性能最优：zero-copy passthrough，无序列化/反序列化开销。

但 filter chain 仍然完整执行——鉴权、权限合并、RPM 限速、余额冻结、用量记录全部活跃。

### 1.3 Aether 行为参考

Aether 的 CLI 架构：

```
CliAdapterBase (api/handlers/base/cli_adapter_base.py)
  ├─ ClaudeCliAdapter   (FORMAT_ID = "claude:cli")
  ├─ OpenAICliAdapter   (FORMAT_ID = "openai:cli")
  ├─ OpenAICompactAdapter (FORMAT_ID = "openai:compact")
  └─ GeminiCliAdapter   (FORMAT_ID = "gemini:cli")

CliMessageHandlerBase (api/handlers/base/cli_handler_base.py)
  mixins: CliRequestMixin, CliStreamMixin, CliSyncMixin,
          CliEventMixin, CliMonitorMixin, CliPrefetchMixin
```

关键属性：
- `mode = ApiMode.PROXY`（透明代理模式）
- `PassthroughRequestBuilder` 构建上行请求
- stream/sync 分支均支持
- 用量从原始响应 JSON 或 SSE 事件中提取（不走 IR）

---

## 2. 设计决策（ADR 系列）

### ADR-CLI-1 FORMAT_ID 注册表

**决策**：定义 4 个 CLI FORMAT_ID，扩展现有 `ApiShape` 枚举。

```rust
// crates/gw-core/src/protocol.rs

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiShape {
    // 现有
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerate,
    // CLI 变体（新增）
    ClaudeCli,       // FORMAT_ID: "claude:cli"
    OpenAiCli,       // FORMAT_ID: "openai:cli"
    OpenAiCompact,   // FORMAT_ID: "openai:compact"
    GeminiCli,       // FORMAT_ID: "gemini:cli"
}

impl ApiShape {
    /// 返回 FORMAT_ID 字符串，兼容 Aether 命名
    pub fn format_id(&self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai:chat",
            Self::OpenAiResponses => "openai:responses",
            Self::AnthropicMessages => "anthropic:messages",
            Self::GeminiGenerate => "gemini:generate",
            Self::ClaudeCli => "claude:cli",
            Self::OpenAiCli => "openai:cli",
            Self::OpenAiCompact => "openai:compact",
            Self::GeminiCli => "gemini:cli",
        }
    }

    /// 此 shape 是否为 CLI 变体
    pub fn is_cli(&self) -> bool {
        matches!(self, Self::ClaudeCli | Self::OpenAiCli
                      | Self::OpenAiCompact | Self::GeminiCli)
    }

    /// CLI shape 到其对应的基础 ProtocolKind 映射
    pub fn protocol_kind(&self) -> ProtocolKind {
        match self {
            Self::OpenAiChat | Self::OpenAiResponses
            | Self::OpenAiCli | Self::OpenAiCompact => ProtocolKind::OpenAi,
            Self::AnthropicMessages | Self::ClaudeCli => ProtocolKind::Anthropic,
            Self::GeminiGenerate | Self::GeminiCli => ProtocolKind::Gemini,
        }
    }
}
```

**与 `ProtocolKind` 的关系**：`ProtocolKind`（3 值）不变，仍用于 key-health、routing 等粗粒度分桶。`ApiShape`（8 值）在 codec 检测和 UsageRecord 中使用，提供细粒度区分。

**替代方案**：
- **A1**：用 `(ProtocolKind, bool)` 元组表示 CLI——过于简化，无法区分 `openai:cli` 与 `openai:compact`。
- **A2**：独立 `CliFormat` 枚举——增加类型数量且需要在多处做 `Either<ApiShape, CliFormat>` 分支。

---

### ADR-CLI-2 透明代理模型（body passthrough，无 IR 转换）

**决策**：CLI 变体的请求体和响应体**不经过** `parse_request → IR → render_request` 通路。网关对 body 只做以下操作：

1. **读取**：`request_filter` 中 `session.read_request_body()` 读入 `ctx.cached_body`；
2. **提取模型名**：从 JSON body 浅层 parse 出 `model` 字段（或从 URL path 提取）；
3. **提取 stream 标志**：从 body `stream` 字段或 path 推断；
4. **转发**：`request_body_filter` 中原样转发（`upstream_request_filter` 中不做 codec rewrite）；
5. **响应透传**：`response_body_filter` 中响应体直通客户端，仅做用量事件扫描。

```
CLI request flow:

client body ──────────────────────────────────────► upstream body
             ↓ (shallow JSON parse: model, stream)
             ctx.model = "claude-sonnet-4-20250514"
             ctx.is_stream = true
             ↓ (header rewrite: adapter)
             vendor auth injected

upstream response ────────────────────────────────► client response
                   ↓ (SSE event scan: usage tokens)
                   ctx.usage_accum += {input, output, cached}
```

**`upstream_request_filter` 中的分支**：

```rust
// crates/gw-proxy/src/filters/upstream.rs (伪代码)

if ctx.client_shape.is_cli() {
    // CLI passthrough: 不做 codec rewrite
    // 仅注入 vendor auth 头、Host、traceparent
    // ctx.cached_body 原样保留
} else {
    // 标准路径：same-protocol passthrough 或 cross-protocol IR rewrite
    ...
}
```

**CLI body 浅层 parse 策略**：

| FORMAT_ID | model 来源 | stream 来源 |
|-----------|-----------|-------------|
| `claude:cli` | body `.model` | body `.stream` |
| `openai:cli` | body `.model` | body `.stream`（默认 true） |
| `openai:compact` | body `.model` | body `.stream`（默认 false，路径隐含） |
| `gemini:cli` | URL path `{model}` | URL path action `streamGenerateContent` vs `generateContent` |

浅层 parse 用 `serde_json::from_slice` 到一个只含 `model` 和 `stream` 字段的小 struct（`#[serde(default)]`），不反序列化完整 body。

```rust
/// 仅提取 CLI 路径所需的最小字段集
#[derive(Deserialize)]
struct CliBodyHints {
    model: Option<String>,
    #[serde(default)]
    stream: Option<bool>,
}
```

**替代方案**：
- **A1**：CLI 也走 IR（标准路径统一）——Aether 已验证这样做会丢失 vendor 专有字段（thinking signatures、cache TTL hints 等），且性能更差。
- **A2**：完全不 parse body（连 model 也不提取）——无法做 model permission 检查和 pricing 查找，计费不可能。

---

### ADR-CLI-3 Filter chain 保留

**决策**：CLI 请求走完整 filter chain，与标准请求相同。没有任何 filter 被跳过。

| Filter 阶段 | CLI 行为 | 与标准路径差异 |
|---|---|---|
| 0. `new_ctx` | 正常初始化 | 无 |
| 1. `early_request_filter` | IP blocklist、stream cap、**CLI protocol detect**、debug | detect 逻辑扩展（见 ADR-CLI-6） |
| 2. `request_filter` | auth、**浅层 body parse**（非 IR parse）、permission merge、balance、RPM、freeze | body 读取后不调 `codec.parse_request`，改为 `CliBodyHints` 浅层提取 |
| 3. `upstream_peer` | candidate filter、strategy pick、HttpPeer | `ApiShape` 传递给 candidate filter 用于 capability 匹配 |
| 4. `upstream_request_filter` | **不做 codec rewrite**、vendor auth 注入 | `cached_body` 原样保留，不经过 `same_protocol_passthrough` 或 IR convert |
| 5. `request_body_filter` | 原样转发 `cached_body` | 无差异 |
| 6. `response_filter` | 状态捕获、failover | 无差异 |
| 7. `response_body_filter` | **CLI 用量扫描**（非标准 SSE parse） | 见 ADR-CLI-4 |
| 8. `logging` | freeze 释放、key-health、**UsageRecord 发射** | UsageRecord 中 `client_protocol` 字段反映 CLI shape |

**关键不变量**：

- `ctx.freeze_guard` 在 step 2 中基于 model pricing 估算创建，CLI 和标准路径相同。
- `ctx.model` 在 step 2 中从浅层 parse 获得（CLI）或从 IR parse 获得（标准），后续 filter 不感知来源差异。
- `UsageRecord.client_protocol` 存储 `ApiShape`（8 值），使 Go billing 能区分 `claude:cli` 与 `anthropic:messages`。

**替代方案**：
- **A1**：跳过 permission merge / RPM（"CLI 是内部工具"）——违反安全原则；CLI 流量也必须受权限控制。
- **A2**：为 CLI 做一个独立的 request handler，绕过 Pingora filter chain——重复代码，维护灾难。

---

### ADR-CLI-4 无 IR 计费（从原始响应提取 token counts）

**决策**：CLI 响应的 token 用量从原始 SSE 事件或非流式 JSON 响应中直接提取，不经过 `parse_response → UsageIR` 通路。

#### 4.1 流式用量提取

每个 CLI 格式有专属的用量事件位置：

**Claude CLI** (`claude:cli`)：

```
event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":150,"cache_read_input_tokens":50,"cache_creation_input_tokens":0}}}

...

event: message_delta
data: {"type":"message_delta","usage":{"output_tokens":342}}

event: message_stop
```

提取规则：
- `message_start.message.usage.input_tokens` → `ctx.usage_accum.input_tokens`
- `message_start.message.usage.cache_read_input_tokens` → `ctx.usage_accum.cache_read_input_tokens`
- `message_start.message.usage.cache_creation_input_tokens` → `ctx.usage_accum.cache_creation_input_tokens`
- `message_delta.usage.output_tokens` → `ctx.usage_accum.output_tokens`

**OpenAI CLI** (`openai:cli`, `openai:compact`)：

```
event: response.completed
data: {"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":200,"input_tokens_details":{"cached_tokens":30}}}}
```

提取规则：
- `response.completed.response.usage.input_tokens` → `ctx.usage_accum.input_tokens`
- `response.completed.response.usage.output_tokens` → `ctx.usage_accum.output_tokens`
- `response.completed.response.usage.input_tokens_details.cached_tokens` → `ctx.usage_accum.cache_read_input_tokens`

**Gemini CLI** (`gemini:cli`)：

```json
{"candidates":[...],"usageMetadata":{"promptTokenCount":120,"candidatesTokenCount":280,"thoughtsTokenCount":50,"cachedContentTokenCount":20}}
```

提取规则：
- `usageMetadata.promptTokenCount` → `ctx.usage_accum.input_tokens`
- `usageMetadata.candidatesTokenCount + usageMetadata.thoughtsTokenCount` → `ctx.usage_accum.output_tokens`
- `usageMetadata.cachedContentTokenCount` → `ctx.usage_accum.cache_read_input_tokens`

#### 4.2 非流式用量提取

非流式 CLI 响应在 `response_body_filter` 的 `end_of_stream` 时一次性 parse 用量字段。结构与流式相同，区别是从完整 JSON 响应体提取而非 SSE 事件。

#### 4.3 CliUsageExtractor trait

```rust
// crates/gw-codec/src/cli/usage.rs

/// 从 CLI 原始响应中提取 token 用量的 trait。
/// 每个 FORMAT_ID 实现一次。
pub trait CliUsageExtractor: Send + Sync {
    /// 从 SSE 事件中增量提取用量。
    /// 返回 Some 表示此事件包含用量信息。
    fn extract_stream_usage(&self, event_type: &str, data: &[u8])
        -> Option<CliUsageDelta>;

    /// 从非流式完整 JSON 响应中一次性提取用量。
    fn extract_response_usage(&self, body: &[u8])
        -> Option<CliUsageFull>;
}

#[derive(Default)]
pub struct CliUsageDelta {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Default)]
pub struct CliUsageFull {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}
```

每个 FORMAT_ID 对应一个实现：

```rust
pub struct ClaudeCliUsageExtractor;
pub struct OpenAiCliUsageExtractor;   // openai:cli + openai:compact 共用
pub struct GeminiCliUsageExtractor;
```

#### 4.4 response_body_filter 中的 CLI 分支

```rust
// crates/gw-proxy/src/filters/response.rs (伪代码)

if ctx.client_shape.is_cli() {
    let extractor = cli_extractor_for(ctx.client_shape);
    if ctx.is_stream {
        // 流式：扫描每个 SSE event boundary
        for event in sse_events(chunk) {
            if let Some(delta) = extractor.extract_stream_usage(event.type, event.data) {
                ctx.usage_accum.merge_delta(delta);
            }
        }
        // body 直通（不改写）
    } else {
        // 非流式：end_of_stream 时一次 parse
        if end_of_stream {
            ctx.upstream_body_accum.extend(chunk);
            let body = ctx.upstream_body_accum.take();
            if let Some(usage) = extractor.extract_response_usage(&body) {
                ctx.usage_accum = usage.into();
            }
        }
        // body 直通
    }
} else {
    // 标准路径（现有逻辑不变）
    ...
}
```

**替代方案**：
- **A1**：CLI 也走 `parse_response → UsageIR`——需要为 CLI 响应定义完整 IR 映射，但 CLI 响应 body 有大量 vendor 专有字段无法映射，且增加不必要的反序列化成本。
- **A2**：不提取 token，直接用 freeze estimate 作为最终用量——billing 不准确，与 Aether 行为不一致。
- **A3**：在 `logging` filter 中提取（延迟 parse）——响应体已流出，非流式可能需要额外缓存；流式事件已经在 `response_body_filter` 扫描。

---

### ADR-CLI-5 CliCodec trait 表面与注册

**决策**：CLI 变体不实现 `ProtocolCodec` trait（无 `parse_request`/`render_request`/`parse_response`/`render_response`）。取而代之的是一个更窄的 `CliCodec` trait，仅覆盖 CLI 通路需要的操作。

```rust
// crates/gw-codec/src/cli/mod.rs

pub mod usage;

use crate::cli::usage::{CliUsageExtractor, CliUsageDelta, CliUsageFull};
use gw_core::protocol::ApiShape;

/// CLI 变体的 codec 接口。
/// 比 ProtocolCodec 更窄——CLI 不做 body parse/render，只做检测 + 用量提取。
pub trait CliCodec: Send + Sync + 'static {
    /// 返回此 codec 对应的 ApiShape
    fn shape(&self) -> ApiShape;

    /// 从请求 body 浅层 parse 出 model 和 stream 标志。
    /// 如果 model/stream 可从 URL path 获得（如 Gemini CLI），body 可以是 None。
    fn extract_hints(&self, path: &str, body: Option<&[u8]>) -> CliHints;

    /// 返回此 FORMAT_ID 的用量提取器
    fn usage_extractor(&self) -> &dyn CliUsageExtractor;

    /// SSE 事件类型列表，用于 TTFT 检测（第一个 delta 类事件）。
    fn ttft_event_types(&self) -> &'static [&'static str];
}

#[derive(Default)]
pub struct CliHints {
    pub model: Option<String>,
    pub stream: Option<bool>,
}
```

**注册**：在 `AnyCodec` 中新增 CLI 变体：

```rust
// crates/gw-codec/src/any.rs

pub enum AnyCodec {
    OpenAi(OpenAi),
    OpenAiResponses(OpenAiResponsesCodec),
    Anthropic(Anthropic),
    Gemini(Gemini),
    // CLI 变体
    ClaudeCli(ClaudeCliCodec),
    OpenAiCli(OpenAiCliCodec),
    OpenAiCompact(OpenAiCompactCodec),
    GeminiCli(GeminiCliCodec),
}
```

CLI 变体的 `AnyCodec` dispatch 在 `parse_request`/`render_request` 等方法上 panic（这些方法不应被 CLI 路径调用），在 `kind()` 返回对应的 `ProtocolKind`，在 `shape()` 返回 CLI 的 `ApiShape`。

**`is_cli()` 方法**：

```rust
impl AnyCodec {
    pub fn is_cli(&self) -> bool {
        matches!(self,
            AnyCodec::ClaudeCli(_) | AnyCodec::OpenAiCli(_)
            | AnyCodec::OpenAiCompact(_) | AnyCodec::GeminiCli(_))
    }

    /// CLI codec 引用。仅 CLI 变体返回 Some。
    pub fn as_cli(&self) -> Option<&dyn CliCodec> {
        match self {
            AnyCodec::ClaudeCli(c) => Some(c),
            AnyCodec::OpenAiCli(c) => Some(c),
            AnyCodec::OpenAiCompact(c) => Some(c),
            AnyCodec::GeminiCli(c) => Some(c),
            _ => None,
        }
    }
}
```

**错误发射**：CLI 变体仍需要发送符合客户端协议的错误响应。CLI 的 `AnyCodec` 在 `render_error` 上委派给对应的基础 codec（如 `ClaudeCli` → `Anthropic::render_error`）。

**替代方案**：
- **A1**：CLI 也实现完整 `ProtocolCodec`——parse/render 方法永远不被调用，是死代码；且为 CLI 定义完整 wire struct 工作量大、价值低。
- **A2**：不扩展 `AnyCodec`，CLI 用独立枚举——需要在 `ReqCtx` 中新增字段或用 `Either`，分支逻辑更复杂。

---

### ADR-CLI-6 检测启发式（如何区分 CLI 与标准请求）

**决策**：CLI 请求在 `early_request_filter` 中通过路径 + 头部组合检测，扩展现有 `AnyCodec::detect` 逻辑。

#### 6.1 检测规则

| FORMAT_ID | 检测条件 | 优先级 |
|-----------|---------|--------|
| `claude:cli` | path = `/v1/messages` AND `Authorization: Bearer *` AND 无 `x-api-key` 头 | 高于 `anthropic:messages` |
| `openai:cli` | path = `/v1/responses` | 独立路径，无歧义 |
| `openai:compact` | path = `/v1/responses/compact` | 独立路径，无歧义 |
| `gemini:cli` | Gemini path 模式 AND (`x-app` 含 "cli" OR User-Agent 含 "gemini-cli"/"geminicli") | 高于 `gemini:generate` |

**Aether 行为对照**：

```python
# Claude CLI detection (Aether)
def build_claude_adapter(request):
    has_bearer = auth_header.lower().startswith("bearer ")
    has_api_key = bool(request.headers.get("x-api-key"))
    if has_bearer and not has_api_key:
        return ClaudeCliAdapter()  # claude:cli
    return ClaudeChatAdapter()     # anthropic:messages

# Gemini CLI detection (Aether)
def _is_cli_request(request):
    x_app = request.headers.get("x-app", "")
    if "cli" in x_app.lower(): return True
    user_agent = request.headers.get("user-agent", "").lower()
    if "geminicli" in user_agent or "gemini-cli" in user_agent: return True
    return False
```

#### 6.2 AnyCodec::detect 扩展

```rust
// crates/gw-codec/src/any.rs

pub fn detect(path: &str, headers: &[(&str, &[u8])], _body_prefix: &[u8]) -> Option<AnyCodec> {
    // 1. OpenAI Compact（最长路径优先）
    if path.starts_with("/v1/responses/compact") {
        return Some(AnyCodec::OpenAiCompact(OpenAiCompactCodec));
    }

    // 2. OpenAI CLI（/v1/responses 但不是 /compact）
    if path.starts_with("/v1/responses") {
        return Some(AnyCodec::OpenAiCli(OpenAiCliCodec));
    }

    // 3. Anthropic / Claude CLI（同路径 /v1/messages，靠头区分）
    if path.starts_with("/v1/messages") {
        let has_bearer = headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("authorization")
            && v.len() >= 7
            && v[..7].eq_ignore_ascii_case(b"Bearer ")
        });
        let has_api_key = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("x-api-key"));

        if has_bearer && !has_api_key {
            return Some(AnyCodec::ClaudeCli(ClaudeCliCodec));
        }
        return Some(AnyCodec::Anthropic(Anthropic));
    }

    // 4. Gemini CLI / standard
    if is_gemini_path(path) {
        let is_cli = headers.iter().any(|(k, v)| {
            (k.eq_ignore_ascii_case("x-app")
             && std::str::from_utf8(v).map_or(false, |s| s.to_lowercase().contains("cli")))
            || (k.eq_ignore_ascii_case("user-agent")
                && std::str::from_utf8(v).map_or(false, |s| {
                    let lower = s.to_lowercase();
                    lower.contains("gemini-cli") || lower.contains("geminicli")
                }))
        });
        if is_cli {
            return Some(AnyCodec::GeminiCli(GeminiCliCodec));
        }
        return Some(AnyCodec::Gemini(Gemini));
    }

    // 5. OpenAI Chat（现有逻辑）
    if path.starts_with("/v1/chat/completions") {
        return Some(AnyCodec::OpenAi(OpenAi));
    }

    // 6. OpenAI Responses（现有，已被上面的 /v1/responses 覆盖，
    //    但保留为 fallback）
    // ...

    None
}
```

**检测顺序不变量**：
- 最长路径前缀优先（`/v1/responses/compact` 在 `/v1/responses` 之前）
- 同路径用头部区分（`/v1/messages` 靠 Bearer vs x-api-key）
- 所有检测仍然 branch-free、no-alloc（header 遍历 O(N)，N = header 数量）

**替代方案**：
- **A1**：独立的 CLI 路径前缀 `/cli/v1/...`——改变客户端 URL，与 Aether 不兼容。
- **A2**：所有 CLI 用 `X-Gateway-Cli: true` 自定义头——要求客户端修改，不透明。
- **A3**：仅靠 User-Agent 检测——UA 可伪造，不适合做安全决策；但用作辅助信号（Gemini CLI）可以接受。

---

### ADR-CLI-7 流式支持

**决策**：CLI 变体同时支持流式和非流式请求，行为与 Aether 一致。

#### 7.1 流式策略矩阵

| FORMAT_ID | 客户端默认 stream | 支持 non-stream | 上游 stream policy |
|-----------|-------------------|-----------------|-------------------|
| `claude:cli` | 由 body `stream` 字段决定 | 是 | 跟随客户端 |
| `openai:cli` | 通常 true | 是 | 强制 stream（Aether: `FORCE_STREAM`） |
| `openai:compact` | 通常 false | 是 | 跟随客户端 |
| `gemini:cli` | 由 path action 决定 | 是 | 跟随 path（`streamGenerateContent` vs `generateContent`） |

#### 7.2 流式 body 直通

CLI 流式路径的 body 直通逻辑：

```
upstream SSE chunk → response_body_filter
  ├─ scan for usage events → merge into ctx.usage_accum
  ├─ scan for TTFT event → stamp ctx.timing.ttft
  └─ forward chunk to client verbatim (zero-copy)
```

与标准路径的差异：标准路径在跨协议时会 re-render SSE 事件（`gw_codec_cross_render_total`）；CLI 路径**永远不 re-render**——事件直通。

#### 7.3 非流式 body 直通

CLI 非流式：

```
upstream response body → response_body_filter
  ├─ accumulate until end_of_stream
  ├─ extract usage from complete JSON body
  └─ forward complete body to client verbatim
```

#### 7.4 stream flag 强制

对于 `openai:cli`（Aether 中 Codex 默认 `FORCE_STREAM`），如果客户端未设置 `stream`，网关**不**强制修改 body。理由：body 透传原则；如果上游要求 stream，应由客户端或 adapter 在头部控制。`openai:cli` 的 Aether `FORCE_STREAM` 是在 adapter 层实现的（provider-side），Rust 网关的 adapter（A-T 系列）承担这个职责。

#### 7.5 错误处理

流式 CLI 请求的错误处理与标准路径相同：
- 上游 5xx/429：`response_filter` 中触发 failover（如果 body 未开始）
- 流式中途断开：`response_body_filter` 中 `response_deadline` 检查
- 用量提取失败：降级为 freeze estimate（`UsageSource::Estimated`）

**替代方案**：
- **A1**：CLI 只支持 stream（简化）——Aether 明确支持 sync CLI 请求（`openai:compact` 默认非流式），不能砍。

---

### ADR-CLI-8 跨协议约束（CLI 不做跨协议 re-render）

**决策**：CLI 变体**禁止**跨协议请求/响应 re-render。

**场景**：当 `ctx.client_shape` 是 CLI 变体（如 `claude:cli`），但 `picked_vendor_key` 的上游是不同协议（如 OpenAI），gateway 应该**拒绝**而非尝试转换。

理由：
1. CLI body 不走 IR，无法做 wire→wire 直转。
2. CLI 响应的 vendor 专有字段无法映射到其他协议。
3. Aether CLI 几乎总是同协议路由（Claude Code → Anthropic、Codex → OpenAI）。

**实现**：在 `upstream_peer`（Phase 3）的 candidate filter 中，CLI 请求额外约束候选 key 必须与 `client_shape.protocol_kind()` 匹配：

```rust
// crates/gw-proxy/src/filters/upstream.rs (伪代码)

if ctx.client_shape.is_cli() {
    let required_kind = ctx.client_shape.protocol_kind();
    candidates.retain(|key| key.accessible_endpoints.contains(&required_kind));
    if candidates.is_empty() {
        return Err(GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL);
    }
}
```

**新增错误码**：`GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL`（503）——CLI 请求找不到同协议上游 key。

**Metric**：`gw_cli_cross_protocol_denied_total{client_shape, required_kind}`

**替代方案**：
- **A1**：允许 CLI 跨协议但在 body passthrough 后 parse 完整 body 做转换——复杂度爆炸，且违反"CLI 不过 IR"原则。
- **A2**：静默降级为标准路径（走 IR）——客户端会收到格式不同的响应，破坏 CLI 工具预期。

---

### ADR-CLI-9 CLI adapter 协作（与现有 A-T 系列的关系）

**决策**：CLI 的 vendor-specific 行为（OAuth 注入、Stainless 头、v1internal URL 重写）仍由现有 adapter 体系（A-T0 trait + A-T1/A-T2 实现）承担。`CliCodec` 只管检测 + 用量提取；adapter 管 header/URL/body 改写。

两者在 filter chain 中的协作时序：

```
early_request_filter:  AnyCodec::detect → ctx.client_codec = ClaudeCli(...)
                       ctx.client_shape = ClaudeCli

request_filter:        CliCodec::extract_hints → ctx.model, ctx.is_stream
                       (标准 auth + permission merge + freeze)

upstream_peer:         candidate filter（CLI 同协议约束）
                       strategy pick → picked_vendor_key

upstream_request_filter:
  adapter.rewrite_auth(ctx)     // OAuth bearer injection
  adapter.rewrite_transport(ctx) // URL rewrite (e.g. v1internal)
  adapter.patch_request(ctx, wire) // 头部注入 (Stainless, x-app)
  // 注意：对 CLI 来说 `wire` 参数不是 WireAnyRequest，
  // 而是 PassthroughBody（原始 bytes）。
  // adapter.patch_request 在 CLI 路径上只操作 headers。
  // body 不变。

response_body_filter:
  CliCodec::usage_extractor → 用量提取
```

**adapter 在 CLI 路径的约束**：
- `patch_request` 不得修改 body（CLI 透传不变量）；adapter 只能改 headers / URL。
- `patch_response` 同样不得修改 response body。
- 如果 adapter 确实需要改 body（罕见），需要在 spec 中显式登记并加 metric `gw_cli_adapter_body_mutation_total`。

**替代方案**：
- **A1**：把 adapter 逻辑合并进 CliCodec——违反职责分离；adapter 是 vendor-specific 的，codec 是协议-specific 的。
- **A2**：CLI 不走 adapter——失去 OAuth、Stainless 头等能力，与 Aether 不兼容。

---

### ADR-CLI-10 Observability 与 Metrics

**决策**：CLI 变体在 observability 表面上通过 `ApiShape` 标签区分，不增加独立的 metric family。

| Metric | CLI 行为 |
|--------|---------|
| `gateway_requests_total{status_class, api_shape}` | 新增 `api_shape` label，CLI 使用 `claude_cli` / `openai_cli` / `openai_compact` / `gemini_cli` |
| `gw_cli_usage_extract_total{shape, source}` | 新增。`source` = `stream_event` \| `response_body` \| `estimated`（提取失败降级） |
| `gw_cli_cross_protocol_denied_total{client_shape}` | 新增。CLI 跨协议拒绝计数 |
| `gw_codec_cross_render_total` | CLI 不触发此 metric（不做 re-render） |
| `gw_codec_passthrough_total{kind}` | CLI 流式事件计入，`kind` = CLI shape |

**UsageRecord 扩展**：

```rust
pub struct UsageRecord {
    // ... 现有字段
    pub client_shape: ApiShape,     // 8 值，取代原 client_protocol
    pub upstream_shape: ApiShape,   // 上游 shape（CLI 时通常 = client_shape）
    pub usage_source: UsageSource,  // Parsed | Estimated | CliExtracted
}

pub enum UsageSource {
    Parsed,         // 标准路径 parse_response
    Estimated,      // 提取失败降级
    CliExtracted,   // CLI 用量提取器
}
```

---

## 3. 请求/响应示例

### 3.1 Claude CLI (`claude:cli`)

**请求**：

```http
POST /v1/messages HTTP/1.1
Authorization: Bearer sk-ant-oauthXXXX
anthropic-version: 2023-06-01
anthropic-beta: claude-code-20250219,interleaved-thinking-2025-05-14
Content-Type: application/json

{"model":"claude-sonnet-4-20250514","max_tokens":16384,"stream":true,
 "messages":[{"role":"user","content":"Explain async/await in Rust"}],
 "thinking":{"type":"enabled","budget_tokens":10000}}
```

**响应（流式，透传）**：

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_01X","model":"claude-sonnet-4-20250514","usage":{"input_tokens":42,"cache_read_input_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me explain..."}}

...

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":523}}

event: message_stop
data: {"type":"message_stop"}
```

### 3.2 OpenAI CLI (`openai:cli`)

**请求**：

```http
POST /v1/responses HTTP/1.1
Authorization: Bearer sk-XXXX
Content-Type: application/json

{"model":"o3","input":[{"role":"user","content":"Write a fibonacci function"}],
 "stream":true,"tools":[{"type":"function","name":"exec","description":"..."}]}
```

**响应（流式，透传）**：

```
event: response.created
data: {"type":"response.created","response":{"id":"resp_01X","model":"o3","status":"in_progress"}}

event: response.output_item.added
data: {"type":"response.output_item.added","item":{"type":"message","content":[]}}

event: response.content_part.delta
data: {"type":"response.content_part.delta","delta":{"type":"text_delta","text":"def fib"}}

...

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_01X","status":"completed","usage":{"input_tokens":85,"output_tokens":142,"input_tokens_details":{"cached_tokens":0}}}}
```

### 3.3 OpenAI Compact (`openai:compact`)

**请求**：

```http
POST /v1/responses/compact HTTP/1.1
Authorization: Bearer sk-XXXX
Content-Type: application/json

{"model":"gpt-4.1","input":"Summarize this text: ...","stream":false}
```

**响应（非流式，透传）**：

```json
{"id":"resp_02Y","model":"gpt-4.1","status":"completed",
 "output":[{"type":"message","content":[{"type":"output_text","text":"The text discusses..."}]}],
 "usage":{"input_tokens":200,"output_tokens":50,"input_tokens_details":{"cached_tokens":0}}}
```

### 3.4 Gemini CLI (`gemini:cli`)

**请求**：

```http
POST /v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse HTTP/1.1
x-app: cli
x-goog-api-key: AIzaXXXX
User-Agent: gemini-cli/0.1.0 (linux; x64) Node.js/v22
Content-Type: application/json

{"contents":[{"role":"user","parts":[{"text":"Hello"}]}],
 "generationConfig":{"maxOutputTokens":8192}}
```

**响应（流式，透传）**：

```
data: {"candidates":[{"content":{"parts":[{"text":"Hi!"}],"role":"model"}}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}}

data: {"candidates":[{"content":{"parts":[{"text":" How can I help?"}]}}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":8,"totalTokenCount":11}}
```

---

## 4. 测试计划

### 4.1 单元测试

| 测试 | 描述 | 文件 |
|------|------|------|
| detect_claude_cli | `/v1/messages` + Bearer 无 x-api-key → `ClaudeCli` | `gw-codec/tests/cli_detect.rs` |
| detect_anthropic_standard | `/v1/messages` + x-api-key → `Anthropic` | 同上 |
| detect_openai_cli | `/v1/responses` → `OpenAiCli` | 同上 |
| detect_openai_compact | `/v1/responses/compact` → `OpenAiCompact` | 同上 |
| detect_gemini_cli_xapp | Gemini path + `x-app: cli` → `GeminiCli` | 同上 |
| detect_gemini_cli_ua | Gemini path + `User-Agent: gemini-cli/0.1.0` → `GeminiCli` | 同上 |
| detect_gemini_standard | Gemini path 无 CLI 信号 → `Gemini` | 同上 |
| claude_usage_stream | `message_start` + `message_delta` → 正确 input/output | `gw-codec/tests/cli_usage.rs` |
| openai_usage_stream | `response.completed` → 正确 input/output/cached | 同上 |
| gemini_usage_stream | `usageMetadata` → 正确 prompt/candidates/thoughts | 同上 |
| claude_usage_nonstream | 完整 Anthropic JSON → 正确用量 | 同上 |
| openai_usage_nonstream | 完整 Responses JSON → 正确用量 | 同上 |
| gemini_usage_nonstream | 完整 Gemini JSON → 正确用量 | 同上 |
| cli_hints_claude | body `{"model":"x","stream":true}` → 正确提取 | `gw-codec/tests/cli_hints.rs` |
| cli_hints_gemini_path | URL `streamGenerateContent` → stream=true, model from path | 同上 |
| api_shape_is_cli | 4 CLI shapes is_cli()=true，4 标准 shapes is_cli()=false | `gw-core/tests/protocol.rs` |
| api_shape_protocol_kind | 所有 8 shapes 映射到正确 ProtocolKind | 同上 |

### 4.2 集成测试

| 测试 | 描述 | 条件 |
|------|------|------|
| e2e_claude_cli_stream | Claude CLI 流式请求→透传→用量提取→UsageRecord | wiremock upstream |
| e2e_openai_cli_stream | OpenAI CLI 流式请求→透传→用量提取 | wiremock |
| e2e_gemini_cli_stream | Gemini CLI 流式→透传→用量 | wiremock |
| e2e_cli_auth_denied | CLI 请求无有效 auth → 401 | — |
| e2e_cli_permission_denied | CLI 请求 model 不在允许列表 → 403 | — |
| e2e_cli_rpm_limited | CLI 请求超 RPM → 429 | — |
| e2e_cli_cross_protocol_denied | Claude CLI 但只有 OpenAI key → 503 `GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL` | — |
| e2e_cli_body_passthrough | 验证请求 body 到达 upstream 完全一致（byte-for-byte） | body hash 比较 |
| e2e_cli_usage_fallback | 响应无用量字段 → UsageSource::Estimated | wiremock 返回空用量 |
| e2e_cli_nonstream | OpenAI Compact 非流式 → 用量提取正确 | wiremock |
| e2e_cli_failover | CLI 流式 upstream 5xx → failover 到同协议 key | wiremock |

### 4.3 grep 验收检查

```bash
# FORMAT_ID 枚举存在
grep -r 'ClaudeCli\|OpenAiCli\|OpenAiCompact\|GeminiCli' crates/gw-core/src/protocol.rs

# CliCodec trait 定义
grep -r 'trait CliCodec' crates/gw-codec/src/

# CliUsageExtractor trait 定义
grep -r 'trait CliUsageExtractor' crates/gw-codec/src/

# detect 中 CLI 分支
grep -r 'ClaudeCli\|OpenAiCli\|OpenAiCompact\|GeminiCli' crates/gw-codec/src/any.rs

# is_cli 方法
grep -r 'fn is_cli' crates/

# 跨协议拒绝错误码
grep -r 'GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL' crates/gw-core/src/error/

# UsageSource::CliExtracted
grep -r 'CliExtracted' crates/

# CLI metric
grep -r 'gw_cli_usage_extract_total\|gw_cli_cross_protocol_denied_total' crates/
```

---

## 5. 迁移与兼容性

### 5.1 与现有 ProtocolKind 的兼容

`ProtocolKind`（3 值）保持不变。所有按 `(KeyId, ProtocolKind)` 分桶的系统（key-health、latency tracker、strategy pick context）不受影响。`ApiShape`（8 值）是更细粒度的类型，仅在 codec 层和 UsageRecord 中使用。

### 5.2 与现有 AnyCodec 的兼容

新增 4 个 `AnyCodec` 变体。现有的 `match` 语句需要加 CLI 分支（Rust 编译器会强制此要求）。

### 5.3 UsageRecord schema 变更

`client_protocol` 字段从 `ProtocolKind`（3 值 string）变为 `ApiShape`（8 值 string）。Go billing consumer 需要识别新值（`"claude_cli"` / `"openai_cli"` / `"openai_compact"` / `"gemini_cli"`）。这是 **Rust ↔ Go 契约变更**，需要同步更新 [`docs/go-contracts/`](../go-contracts/main.md)。

### 5.4 分阶段交付

1. **Phase 1**：`gw-core` — `ApiShape` 枚举扩展 + `is_cli()` / `protocol_kind()` / `format_id()`
2. **Phase 2**：`gw-codec` — `CliCodec` trait + 4 个 codec 实现 + `AnyCodec` 扩展 + detect 扩展
3. **Phase 3**：`gw-proxy` — filter chain CLI 分支（request_filter 浅层 parse、upstream_request_filter passthrough、response_body_filter 用量提取）
4. **Phase 4**：`gw-ledger` — UsageRecord schema 变更 + `UsageSource::CliExtracted`
5. **Phase 5**：集成测试 + soak 验证

---

## 6. 未决问题

| # | 问题 | 影响 |
|---|------|------|
| 1 | `openai:compact` 的 `/v1/responses/compact` 路径是否在 OpenAI 官方 API 中存在，还是 Aether 自定义？需确认上游路由 | 影响 detect + adapter |
| 2 | Gemini CLI 的 `v1internal` URL 重写在 adapter（A-T2）中处理，但 detect 阶段看到的是标准 Gemini path；是否需要在 detect 中区分 `v1internal` 路径？ | 影响 detect 精度 |
| 3 | Claude CLI 的 `anthropic-beta` header 列表（`claude-code-20250219` 等）是否应参与检测？当前只靠 Bearer + 无 x-api-key | 影响 false positive |
| 4 | CLI 流量是否需要进入 `usage:detailed` archive？透传 body 意味着 5 个 surface 的 wire bytes 与标准路径格式不同 | 影响 DetailedArchive |
| 5 | `openai:cli` 的 `FORCE_STREAM` policy 是否应提升到 gateway 层（在 request_filter 中强制设 `ctx.is_stream = true`），还是保留在 adapter 层？ | 影响 freeze estimate 准确度 |
