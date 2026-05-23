//! Gemini wire-format structs (`generateContent`).
//!
//! Request side: `contents (role + parts) / systemInstruction /
//! generationConfig.{maxOutputTokens,temperature}`.
//!
//! Response side adds `GeminiResponse` (`candidates[]`, `usageMetadata`,
//! `modelVersion`).
//!
//! Parts are an `#[serde(untagged)]` enum — Gemini's bare-shape parts arrive
//! without a discriminator tag (`{"text":"..."}` or `{"functionCall": {...}}`)
//! so untagged dispatch matches the wire. Function call `args` and
//! `functionResponse.response` are held as `serde_json::Value` rather than
//! `Cow<'a, RawValue>` — `RawValue` inside `#[serde(untagged)]` triggers the
//! same content-buffering deserialise path that broke the
//! internally-tagged-enum case in T4-C. `Value` round-trips through
//! `via.rs::value_to_raw` when the IR's `RawValue`-backed slots need to be
//! reconstructed.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiRequest<'a> {
    /// Gemini omits an explicit `model` field in the request body — the
    /// model is in the URL path. We still carry it through the IR so callers
    /// can round-trip the value through the request struct (e.g. for cross-
    /// protocol translation). It is `#[serde(skip)]` on the wire.
    #[serde(skip)]
    pub model: Cow<'a, str>,

    #[serde(borrow)]
    pub contents: Vec<GeminiContent<'a>>,

    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GeminiSystemInstruction<'a>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiContent<'a> {
    /// `"user"` or `"model"` per the Gemini spec. Optional on the wire —
    /// the streaming intermediate frames and some non-stream candidate
    /// shapes omit `role` (treated as `"model"` by Gemini's clients).
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub role: Option<Cow<'a, str>>,
    #[serde(borrow)]
    pub parts: Vec<GeminiPart<'a>>,
}

/// Gemini Part — untagged because the wire delivers each part as a bare-shape
/// object keyed by its single relevant field name (`text`, `inlineData`,
/// `fileData`, `functionCall`, `functionResponse`). Order matters for
/// deserialisation: more-specific shapes precede more-permissive ones so
/// untagged dispatch picks the right variant.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum GeminiPart<'a> {
    /// Inline binary data (e.g. base64 image). Kept for multimodal
    /// completeness even though the IR doesn't currently surface it through
    /// the response side.
    InlineData {
        #[serde(borrow, rename = "inlineData")]
        inline_data: GeminiInlineData<'a>,
    },
    /// File reference (e.g. uploaded via the Files API).
    FileData {
        #[serde(borrow, rename = "fileData")]
        file_data: GeminiFileData<'a>,
    },
    /// Assistant tool call. Gemini's wire shape is `{"functionCall":{"name":..,"args":..}}`.
    FunctionCall {
        #[serde(borrow, rename = "functionCall")]
        function_call: GeminiFunctionCall<'a>,
    },
    /// Tool result reply. Wire shape: `{"functionResponse":{"name":..,"response":..}}`.
    FunctionResponse {
        #[serde(borrow, rename = "functionResponse")]
        function_response: GeminiFunctionResponse<'a>,
    },
    /// Free text. Must come LAST in the untagged enum so the structured
    /// variants above are tried first; otherwise an empty `{"text":""}`
    /// shape could shadow a function-call payload that happens to deserialise
    /// loosely.
    Text {
        #[serde(borrow)]
        text: Cow<'a, str>,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiInlineData<'a> {
    #[serde(borrow)]
    pub mime_type: Cow<'a, str>,
    #[serde(borrow)]
    pub data: Cow<'a, str>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiFileData<'a> {
    #[serde(borrow)]
    pub mime_type: Cow<'a, str>,
    #[serde(borrow)]
    pub file_uri: Cow<'a, str>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GeminiFunctionCall<'a> {
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    /// Arguments object — held as `serde_json::Value` (see module docstring).
    #[serde(default)]
    pub args: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GeminiFunctionResponse<'a> {
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    /// Tool-result payload — also held as `serde_json::Value`.
    #[serde(default)]
    pub response: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiSystemInstruction<'a> {
    #[serde(borrow)]
    pub parts: Vec<GeminiPart<'a>>,
}

#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GeminiGenerationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

// ── Non-streaming response ────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiResponse<'a> {
    #[serde(default, borrow)]
    pub candidates: Vec<GeminiCandidate<'a>>,

    /// Optional only on streaming-intermediate frames; the non-stream
    /// terminal response always includes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_metadata: Option<GeminiUsage>,

    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub model_version: Option<Cow<'a, str>>,

    /// Long-running-operation marker: `name: "operations/<uuid>"` on async
    /// jobs. Non-LRO chat responses omit this; the response filter promotes
    /// the value into `ctx.operation_id` when present (ADR-G5-1).
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub name: Option<Cow<'a, str>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiCandidate<'a> {
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub content: Option<GeminiContent<'a>>,

    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<Cow<'a, str>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    // safetyRatings intentionally omitted in v2 — the IR has no per-rating
    // slot and the wire string is opaque; revisit if a cross-protocol
    // consumer needs it.
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiUsage {
    #[serde(default)]
    pub prompt_token_count: u32,
    #[serde(default)]
    pub candidates_token_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_content_token_count: Option<u32>,
    /// Gemini sometimes reports the totals on the wire; we accept and
    /// re-emit but the IR side derives its own total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_token_count: Option<u32>,
}
