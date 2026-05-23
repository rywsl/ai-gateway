//! OpenAI wire-format structs.
//!
//! The actual `From` / `TryFrom` impls live in `via.rs` (hand-written for
//! v1). Codec S1 v2 (`wire_convert_matrix!`, Phase 2) will reintroduce
//! attribute-driven codegen at that boundary.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiRequest<'a> {
    #[serde(borrow)]
    pub model: Cow<'a, str>,

    #[serde(borrow)]
    pub messages: Vec<OpenAiMessage<'a>>,

    #[serde(default)]
    pub stream: bool,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<Cow<'a, str>>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logit_bias: Option<HashMap<String, f32>>,

    /// Explicit tools field — replaces v1's round-trip through `extra`.
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<OpenAiTool<'a>>>,

    /// Explicit tool_choice field — replaces v1's round-trip through `extra`.
    /// OpenAI accepts either a string (`"auto"`/`"none"`/`"required"`) or an
    /// object (`{"type":"function","function":{"name":"..."}}`); we hold the
    /// raw shape opaquely so both pass through losslessly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Box<RawValue>>,

    /// Vendor-pass-through extras. Keys borrow from the request body when
    /// possible (`Cow<'a, str>`); owned only when constructed at render time.
    #[serde(flatten)]
    pub extra: HashMap<Cow<'a, str>, Box<RawValue>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum OpenAiMessage<'a> {
    System {
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
    User {
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
    Assistant {
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        content: Option<Cow<'a, str>>,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<OpenAiToolCall<'a>>>,
    },
    Tool {
        #[serde(borrow)]
        tool_call_id: Cow<'a, str>,
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OpenAiToolCall<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    #[serde(rename = "type", borrow)]
    pub ty: Cow<'a, str>,
    #[serde(borrow)]
    pub function: OpenAiFunctionCall<'a>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OpenAiFunctionCall<'a> {
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    /// Arguments are kept as a JSON string per the OpenAI spec.
    #[serde(borrow)]
    pub arguments: Cow<'a, str>,
}

/// OpenAI tool declaration as used in `OpenAiRequest.tools`.
///
/// Minimal Cow/RawValue-backed shape — the `function.parameters` JSON Schema
/// is held opaquely so we round-trip arbitrary user-provided shapes.
#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiTool<'a> {
    #[serde(rename = "type", borrow)]
    pub ty: Cow<'a, str>,
    #[serde(borrow)]
    pub function: OpenAiToolFunction<'a>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiToolFunction<'a> {
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub description: Option<Cow<'a, str>>,
    /// JSON Schema for the tool's arguments — held opaquely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Box<RawValue>>,
}

// ─── Non-streaming response wire ──────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiResponse<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    /// Always `"chat.completion"` on the wire.
    #[serde(borrow)]
    pub object: Cow<'a, str>,
    #[serde(default)]
    pub created: u64,
    #[serde(borrow)]
    pub model: Cow<'a, str>,
    #[serde(borrow)]
    pub choices: Vec<OpenAiChoice<'a>>,
    #[serde(default)]
    pub usage: Option<OpenAiUsage>,
    /// Pass-through unknown vendor extras (e.g. `system_fingerprint`).
    #[serde(flatten)]
    pub extra: HashMap<Cow<'a, str>, Box<RawValue>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiChoice<'a> {
    pub index: u32,
    #[serde(borrow)]
    pub message: OpenAiMessage<'a>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<Cow<'a, str>>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct OpenAiUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<OpenAiCompletionTokensDetails>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct OpenAiPromptTokensDetails {
    #[serde(default)]
    pub cached_tokens: u32,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct OpenAiCompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u32,
}

// ─── Streaming SSE payload wire ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiStreamPayload<'a> {
    #[serde(default, borrow)]
    pub choices: Vec<OpenAiStreamChoice<'a>>,
    #[serde(default)]
    pub usage: Option<OpenAiStreamUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiStreamChoice<'a> {
    #[serde(default, borrow)]
    pub delta: OpenAiStreamDelta<'a>,
    #[serde(default, borrow)]
    pub finish_reason: Option<Cow<'a, str>>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct OpenAiStreamDelta<'a> {
    #[serde(default, borrow)]
    pub content: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    pub tool_calls: Option<Vec<OpenAiStreamToolCall<'a>>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiStreamToolCall<'a> {
    pub index: u32,
    #[serde(default, borrow)]
    pub id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    pub function: Option<OpenAiStreamFunction<'a>>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct OpenAiStreamFunction<'a> {
    #[serde(default, borrow)]
    pub name: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    pub arguments: Option<Cow<'a, str>>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct OpenAiStreamUsage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
}
