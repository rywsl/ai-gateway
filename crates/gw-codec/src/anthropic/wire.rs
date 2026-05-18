//! Anthropic wire-format structs (Messages API, v1).
//!
//! v1 scope: `model / messages (user|assistant) / system / max_tokens /
//! temperature / stream`. Tool use is not yet round-tripped through here.

use gw_codec_macros::IrConvert;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Deserialize, Serialize, IrConvert)]
#[ir(target = "RequestIR", direction = "both")]
pub struct AnthropicRequest<'a> {
    #[ir(map = "model")]
    #[serde(borrow)]
    pub model: Cow<'a, str>,

    #[ir(map = "messages", via = "anthropic_msg_vec")]
    #[serde(borrow)]
    pub messages: Vec<AnthropicMessage<'a>>,

    /// Top-level system prompt. Round-tripped through the IR as the first
    /// `MessageIR::System` entry.
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub system: Option<Cow<'a, str>>,

    /// Anthropic requires `max_tokens` (no default). We render it on the way
    /// out — if missing from IR we fall back to a permissive ceiling.
    #[ir(map = "max_tokens")]
    pub max_tokens: u32,

    #[ir(map = "temperature")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[ir(map = "stream", default = "false")]
    #[serde(default)]
    pub stream: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum AnthropicMessage<'a> {
    User {
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
    Assistant {
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
}
