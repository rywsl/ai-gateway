//! OpenAI wire-format structs.
//!
//! `#[derive(IrConvert)]` is present for documentation/forward-compatibility:
//! the actual `From` / `TryFrom` impls live in `via.rs` (hand-written for v1,
//! see `gw-codec-macros/src/lib.rs` for the rationale).

use gw_codec_macros::IrConvert;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize, IrConvert)]
#[ir(target = "RequestIR", direction = "both")]
pub struct OpenAiRequest<'a> {
    #[ir(map = "model")]
    #[serde(borrow)]
    pub model: Cow<'a, str>,

    #[ir(map = "messages", via = "openai_msg_vec")]
    #[serde(borrow)]
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

    #[ir(map = "metadata.openai_logit_bias")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logit_bias: Option<HashMap<String, f32>>,

    #[ir(extensions)]
    #[serde(flatten)]
    pub extra: HashMap<String, Box<RawValue>>,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiToolCall<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    #[serde(rename = "type", borrow)]
    pub ty: Cow<'a, str>,
    #[serde(borrow)]
    pub function: OpenAiFunctionCall<'a>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiFunctionCall<'a> {
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    /// Arguments are kept as a JSON string per the OpenAI spec.
    #[serde(borrow)]
    pub arguments: Cow<'a, str>,
}
