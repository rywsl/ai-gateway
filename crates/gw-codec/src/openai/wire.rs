//! OpenAI wire-format structs.
//!
//! The actual `From` / `TryFrom` impls live in `via.rs`.

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

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<OpenAiResponseFormat<'a>>,

    /// Reasoning effort for o-series models (`"low"`, `"medium"`, `"high"`).
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<Cow<'a, str>>,

    /// Service tier preference (`"auto"`, `"default"`, `"flex"`).
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<Cow<'a, str>>,

    /// Output modalities — e.g. `["text", "audio"]`.
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<Vec<Cow<'a, str>>>,

    /// Audio output configuration (voice, format).
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub audio: Option<OpenAiAudioConfig<'a>>,

    /// Web search configuration (opaque passthrough).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_options: Option<Box<RawValue>>,

    /// Predicted output content (opaque passthrough).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prediction: Option<Box<RawValue>>,

    /// Vendor-pass-through extras. Keys borrow from the request body when
    /// possible (`Cow<'a, str>`); owned only when constructed at render time.
    #[serde(flatten)]
    pub extra: HashMap<Cow<'a, str>, Box<RawValue>>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum OpenAiMessage<'a> {
    System {
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
    Developer {
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
    User {
        #[serde(borrow)]
        content: OpenAiUserContent<'a>,
    },
    Assistant {
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        content: Option<Cow<'a, str>>,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<OpenAiToolCall<'a>>>,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        refusal: Option<Cow<'a, str>>,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        annotations: Option<Vec<OpenAiAnnotation<'a>>>,
        #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
        audio: Option<OpenAiAudioResponse<'a>>,
    },
    Tool {
        #[serde(borrow)]
        tool_call_id: Cow<'a, str>,
        #[serde(borrow)]
        content: Cow<'a, str>,
    },
}

// ─── User multimodal content ──────────────────────────────────────────────

#[derive(Clone, Debug)]
pub enum OpenAiUserContent<'a> {
    Text(Cow<'a, str>),
    Parts(Vec<OpenAiContentPart<'a>>),
}

impl Serialize for OpenAiUserContent<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Text(s) => s.serialize(serializer),
            Self::Parts(parts) => parts.serialize(serializer),
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for OpenAiUserContent<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de;

        struct UserContentVisitor;

        impl<'de> de::Visitor<'de> for UserContentVisitor {
            type Value = OpenAiUserContent<'de>;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string or array of content parts")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(OpenAiUserContent::Text(Cow::Owned(v.to_owned())))
            }

            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(OpenAiUserContent::Text(Cow::Borrowed(v)))
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(OpenAiUserContent::Text(Cow::Owned(v)))
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                let parts: Vec<OpenAiContentPart<'de>> =
                    de::Deserialize::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
                Ok(OpenAiUserContent::Parts(parts))
            }
        }

        deserializer.deserialize_any(UserContentVisitor)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiContentPart<'a> {
    Text {
        #[serde(borrow)]
        text: Cow<'a, str>,
    },
    ImageUrl {
        #[serde(borrow)]
        image_url: OpenAiImageUrl<'a>,
    },
    InputAudio {
        #[serde(borrow)]
        input_audio: OpenAiInputAudio<'a>,
    },
    File {
        #[serde(borrow)]
        file: OpenAiFileRef<'a>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiImageUrl<'a> {
    #[serde(borrow)]
    pub url: Cow<'a, str>,
    /// `"auto"`, `"low"`, `"high"`.
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Cow<'a, str>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiInputAudio<'a> {
    /// Base64-encoded audio data.
    #[serde(borrow)]
    pub data: Cow<'a, str>,
    /// e.g. `"wav"`, `"mp3"`.
    #[serde(borrow)]
    pub format: Cow<'a, str>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiFileRef<'a> {
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<Cow<'a, str>>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<Cow<'a, str>>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<Cow<'a, str>>,
}

// ─── Response annotations ─────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiAnnotation<'a> {
    #[serde(rename = "type", borrow)]
    pub ty: Cow<'a, str>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub url_citation: Option<OpenAiUrlCitation<'a>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiUrlCitation<'a> {
    pub start_index: u32,
    pub end_index: u32,
    #[serde(borrow)]
    pub title: Cow<'a, str>,
    #[serde(borrow)]
    pub url: Cow<'a, str>,
}

// ─── Audio config / response ──────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiAudioConfig<'a> {
    #[serde(borrow)]
    pub voice: Cow<'a, str>,
    #[serde(borrow)]
    pub format: Cow<'a, str>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAiAudioResponse<'a> {
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    #[serde(borrow)]
    pub data: Cow<'a, str>,
    pub expires_at: u64,
    #[serde(borrow)]
    pub transcript: Cow<'a, str>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpenAiResponseFormat<'a> {
    Text,
    JsonObject,
    JsonSchema {
        #[serde(borrow)]
        json_schema: OpenAiJsonSchema<'a>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OpenAiJsonSchema<'a> {
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub name: Option<Cow<'a, str>>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub description: Option<Cow<'a, str>>,
    #[serde(with = "raw_json_box")]
    pub schema: Box<RawValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
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

/// OpenAI tool declaration as used in `OpenAiRequest.tools`.
///
/// Supports both function tools (`{"type":"function","function":{...}}`) and
/// custom/server tools (`{"type":"web_search_preview",...}`).  For custom tools
/// `function` is `None` and extra keys land in `extra`.
#[derive(Debug, Deserialize, Serialize)]
pub struct OpenAiTool<'a> {
    #[serde(rename = "type", borrow)]
    pub ty: Cow<'a, str>,
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub function: Option<OpenAiToolFunction<'a>>,
    #[serde(flatten)]
    pub extra: HashMap<Cow<'a, str>, serde_json::Value>,
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

#[derive(Debug, Serialize, Deserialize)]
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
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<Cow<'a, str>>,
    /// Pass-through unknown vendor extras (e.g. `system_fingerprint`).
    #[serde(flatten)]
    pub extra: HashMap<Cow<'a, str>, Box<RawValue>>,
}

#[derive(Debug, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_tokens: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct OpenAiCompletionTokensDetails {
    #[serde(default)]
    pub reasoning_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_prediction_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_prediction_tokens: Option<u32>,
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
    #[serde(default, borrow)]
    pub refusal: Option<Cow<'a, str>>,
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
    #[serde(default)]
    pub completion_tokens_details: Option<OpenAiCompletionTokensDetails>,
}
