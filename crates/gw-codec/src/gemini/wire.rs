//! Gemini wire-format structs (`generateContent`, v1).
//!
//! v1 scope: `contents (role + parts.text) / systemInstruction /
//! generationConfig.{maxOutputTokens,temperature}`.

use gw_codec_macros::IrConvert;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Deserialize, Serialize, IrConvert)]
#[ir(target = "RequestIR", direction = "both")]
#[serde(rename_all = "camelCase")]
pub struct GeminiRequest<'a> {
    /// Gemini omits an explicit `model` field in the request body — the
    /// model is in the URL path. We still carry it through the IR so callers
    /// can round-trip the value through the request struct (e.g. for cross-
    /// protocol translation). It is `#[serde(skip)]` on the wire.
    #[serde(skip)]
    pub model: Cow<'a, str>,

    #[ir(map = "messages", via = "gemini_contents")]
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
    /// `"user"` or `"model"` per the Gemini spec.
    #[serde(borrow)]
    pub role: Cow<'a, str>,
    #[serde(borrow)]
    pub parts: Vec<GeminiPart<'a>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiPart<'a> {
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub text: Option<Cow<'a, str>>,
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
