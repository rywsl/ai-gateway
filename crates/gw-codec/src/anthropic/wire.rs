//! Anthropic wire-format structs (Messages API).
//!
//! Request side accepts the array-of-blocks content shape: each `User` or
//! `Assistant` message holds a `Vec<AnthropicContent>` whose tag is the
//! block `type` (`text` / `image` / `thinking` / `tool_use` / `tool_result`).
//! The single-string convenience form is normalised to a single
//! `AnthropicContent::Text` on the way in by the `parse_request` path
//! through the IR — the wire structs themselves require the array shape.
//!
//! Response side adds `AnthropicResponse` (non-stream) plus the streaming
//! event-typed payload taxonomy under `AnthropicStreamEvent`.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::borrow::Cow;

#[derive(Debug, Deserialize, Serialize)]
pub struct AnthropicRequest<'a> {
    #[serde(borrow)]
    pub model: Cow<'a, str>,

    #[serde(borrow)]
    pub messages: Vec<AnthropicMessage<'a>>,

    /// Top-level system prompt. Round-tripped through the IR as the first
    /// `MessageIR::System` entry.
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub system: Option<Cow<'a, str>>,

    /// Anthropic requires `max_tokens` (no default). We render it on the way
    /// out — if missing from IR we fall back to a permissive ceiling.
    pub max_tokens: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinkingConfig<'a>>,

    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub output_config: Option<AnthropicOutputConfig<'a>>,

    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<Cow<'a, str>>,

    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicThinkingConfig<'a> {
    Enabled {
        budget_tokens: u32,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        display: Option<Cow<'a, str>>,
    },
    Adaptive {
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        display: Option<Cow<'a, str>>,
    },
    Disabled,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AnthropicOutputConfig<'a> {
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<serde_json::Value>,
}

/// Anthropic content blocks share a `Vec<AnthropicContent>` payload regardless
/// of role; deserialization tolerates the legacy string-only convenience form
/// via `OneOrMany`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum AnthropicMessage<'a> {
    User {
        #[serde(borrow, with = "content_blocks")]
        content: Vec<AnthropicContent<'a>>,
    },
    Assistant {
        #[serde(borrow, with = "content_blocks")]
        content: Vec<AnthropicContent<'a>>,
    },
}

/// One content block. `tag = "type"` matches Anthropic's array-of-blocks
/// shape. Opaque JSON sub-objects (`Image.source`, `ToolUse.input`,
/// `ToolResult.content`) are held as `serde_json::Value`. This is the only
/// shape that round-trips through `serde`'s internally-tagged enum
/// machinery — `Box<RawValue>` would short-circuit the tag dispatch by
/// triggering `deserialize_any`, and `Cow<'a, RawValue>` hits the same path
/// inside `serde`'s content-buffering visitor. `Value` is owned but
/// composes cleanly; the IR's `RawValue`-backed slots are reconstructed
/// in `via.rs`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicContent<'a> {
    Text {
        #[serde(borrow)]
        text: Cow<'a, str>,
    },
    Image {
        /// Opaque `{type, media_type, data}` (b64) or `{type, url}` object.
        source: serde_json::Value,
    },
    Thinking {
        #[serde(borrow)]
        thinking: Cow<'a, str>,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        signature: Option<Cow<'a, str>>,
    },
    ToolUse {
        #[serde(borrow)]
        id: Cow<'a, str>,
        #[serde(borrow)]
        name: Cow<'a, str>,
        /// Arguments object — held as a `serde_json::Value` so any JSON shape
        /// round-trips through the internally-tagged enum machinery.
        input: serde_json::Value,
    },
    ToolResult {
        #[serde(borrow)]
        tool_use_id: Cow<'a, str>,
        /// `content` may be a string, an array of blocks, or omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    #[serde(rename = "server_tool_use")]
    ServerToolUse(ServerToolUseBlock<'a>),
    #[serde(rename = "web_search_tool_result")]
    WebSearchToolResult(ServerToolResultBlock<'a>),
    #[serde(rename = "web_fetch_tool_result")]
    WebFetchToolResult(ServerToolResultBlock<'a>),
    #[serde(rename = "code_execution_tool_result")]
    CodeExecutionToolResult(ServerToolResultBlock<'a>),
    #[serde(rename = "bash_code_execution_tool_result")]
    BashCodeExecutionToolResult(ServerToolResultBlock<'a>),
    #[serde(rename = "text_editor_code_execution_tool_result")]
    TextEditorCodeExecutionToolResult(ServerToolResultBlock<'a>),
    #[serde(rename = "tool_search_tool_result")]
    ToolSearchToolResult(ServerToolResultBlock<'a>),
    #[serde(rename = "mcp_tool_use")]
    McpToolUse(ServerToolUseBlock<'a>),
    #[serde(rename = "mcp_tool_result")]
    McpToolResult(ServerToolResultBlock<'a>),
    #[serde(rename = "container_upload")]
    ContainerUpload(ContainerUploadBlock<'a>),
    Document {
        #[serde(borrow)]
        source: DocumentSource<'a>,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        title: Option<Cow<'a, str>>,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        context: Option<Cow<'a, str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl<'a>>,
    },
    #[serde(rename = "search_result")]
    SearchResult {
        #[serde(borrow)]
        source: Cow<'a, str>,
        #[serde(borrow)]
        title: Cow<'a, str>,
        #[serde(borrow)]
        content: Vec<AnthropicContent<'a>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<AnthropicCacheControl<'a>>,
    },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking {
        #[serde(borrow)]
        data: Cow<'a, str>,
    },
    #[serde(other)]
    Unknown,
}

/// Anthropic `cache_control` annotation block. Carried on content blocks
/// that participate in prompt caching (document, search_result, etc.).
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AnthropicCacheControl<'a> {
    #[serde(borrow, rename = "type")]
    pub kind: Cow<'a, str>,
}

/// Source object for the `document` content block.
/// Discriminated on `"type"`: `base64`, `text`, `url`, or `content`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocumentSource<'a> {
    Base64 {
        #[serde(borrow)]
        media_type: Cow<'a, str>,
        #[serde(borrow)]
        data: Cow<'a, str>,
    },
    Text {
        #[serde(borrow)]
        text: Cow<'a, str>,
    },
    Url {
        #[serde(borrow)]
        url: Cow<'a, str>,
    },
    Content {
        #[serde(borrow)]
        content: Vec<AnthropicContent<'a>>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServerToolUseBlock<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    #[serde(with = "raw_json_box")]
    pub input: Box<RawValue>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServerToolResultBlock<'a> {
    #[serde(default, borrow)]
    pub id: Cow<'a, str>,
    #[serde(default, borrow)]
    pub name: Cow<'a, str>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ContainerUploadBlock<'a> {
    #[serde(borrow)]
    pub file_id: Cow<'a, str>,
}

mod raw_json_box {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::value::RawValue;

    pub fn serialize<S>(value: &RawValue, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Box<RawValue>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let json = serde_json::to_string(&value).map_err(serde::de::Error::custom)?;
        RawValue::from_string(json).map_err(serde::de::Error::custom)
    }
}

// ── Non-streaming response ─────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
pub struct AnthropicResponse<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    /// Always `"message"` on the wire.
    #[serde(borrow, rename = "type")]
    pub ty: Cow<'a, str>,
    #[serde(borrow)]
    pub role: Cow<'a, str>,
    #[serde(borrow)]
    pub model: Cow<'a, str>,
    #[serde(borrow)]
    pub content: Vec<AnthropicContent<'a>>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<Cow<'a, str>>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AnthropicUsage>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<Cow<'a, str>>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub inference_geo: Option<Cow<'a, str>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct AnthropicUsage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
    /// Anthropic exposes `cache_creation_input_tokens` distinct from the
    /// `cache_read` counter; maps to `UsageIR.cache_creation_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// Detailed cache creation breakdown by TTL tier (5-minute vs 1-hour
    /// ephemeral caching).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation: Option<AnthropicCacheCreationDetails>,
    /// Server-side tool usage counts (web_search, web_fetch requests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<AnthropicServerToolUsage>,
}

/// Detailed cache creation breakdown by TTL tier. Anthropic surfaces
/// per-tier token counts when prompt caching uses different TTL buckets.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct AnthropicCacheCreationDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_1h_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_5m_input_tokens: Option<u32>,
}

/// Server-side tool usage counters surfaced inside the `usage` block.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct AnthropicServerToolUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_requests: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_fetch_requests: Option<u32>,
}

// ── Streaming event payloads ───────────────────────────────────────────────

/// Anthropic SSE event payload. The `type` discriminant matches the
/// `event: <type>` framing header — we keep both in sync but rely on the
/// payload's own `type` field to dispatch so producers that omit the header
/// still parse.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicStreamEvent<'a> {
    MessageStart {
        #[serde(borrow)]
        message: AnthropicStreamMessageStart<'a>,
    },
    ContentBlockStart {
        index: u32,
        #[serde(borrow)]
        content_block: AnthropicContent<'a>,
    },
    ContentBlockDelta {
        index: u32,
        #[serde(borrow)]
        delta: AnthropicStreamDelta<'a>,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        #[serde(borrow)]
        delta: AnthropicStreamMessageDelta<'a>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<AnthropicUsage>,
    },
    MessageStop,
    Ping,
    Error {
        #[serde(borrow)]
        error: AnthropicStreamErrorPayload<'a>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AnthropicStreamMessageStart<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub model: Option<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicStreamDelta<'a> {
    TextDelta {
        #[serde(borrow)]
        text: Cow<'a, str>,
    },
    ThinkingDelta {
        #[serde(borrow)]
        thinking: Cow<'a, str>,
    },
    SignatureDelta {
        #[serde(borrow)]
        signature: Cow<'a, str>,
    },
    InputJsonDelta {
        #[serde(borrow)]
        partial_json: Cow<'a, str>,
    },
    CitationsDelta {
        /// Opaque citation object — the internal structure is complex and
        /// vendor-specific (page/char/document references). Held as `Value`
        /// because `Box<RawValue>` cannot deserialize inside an
        /// internally-tagged enum.
        citation: serde_json::Value,
    },
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct AnthropicStreamMessageDelta<'a> {
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<Cow<'a, str>>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<Cow<'a, str>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AnthropicStreamErrorPayload<'a> {
    #[serde(borrow, rename = "type")]
    pub ty: Cow<'a, str>,
    #[serde(borrow)]
    pub message: Cow<'a, str>,
}

// ── Custom serde for the legacy single-string content shape ────────────────
//
// Anthropic's API accepts `content` as either a bare string or an array of
// blocks. On parse we normalise both into `Vec<AnthropicContent>` so the rest
// of the codec can rely on a single shape. On render we always emit the
// array form, which is the canonical Anthropic body shape for any non-text
// block.

mod content_blocks {
    use super::AnthropicContent;
    use serde::de::{Deserializer, Error};
    use serde::ser::Serializer;
    use serde::{Deserialize, Serialize};
    use std::borrow::Cow;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany<'a> {
        Many(#[serde(borrow)] Vec<AnthropicContent<'a>>),
        One(#[serde(borrow)] Cow<'a, str>),
    }

    pub fn deserialize<'de: 'a, 'a, D>(d: D) -> Result<Vec<AnthropicContent<'a>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        match OneOrMany::deserialize(d).map_err(D::Error::custom)? {
            OneOrMany::Many(v) => Ok(v),
            OneOrMany::One(s) => Ok(vec![AnthropicContent::Text { text: s }]),
        }
    }

    pub fn serialize<S>(v: &[AnthropicContent<'_>], s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        v.serialize(s)
    }
}
