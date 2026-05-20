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
// TODO(v2): add explicit tools/tool_choice fields; for v1 they round-trip through `extra`.
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
