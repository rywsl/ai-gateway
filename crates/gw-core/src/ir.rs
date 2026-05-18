use crate::error::ErrorIR;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct UntouchedFlags(pub u32);
impl UntouchedFlags {
    pub const ALL_UNTOUCHED: Self = Self(u32::MAX);
}

#[derive(Clone, Debug)]
pub struct RequestIR<'a> {
    pub model: Cow<'a, str>,
    pub messages: Vec<MessageIR<'a>>,
    pub stream: bool,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
    pub stop: Option<Vec<Cow<'a, str>>>,
    pub seed: Option<u64>,
    pub tools: Option<Vec<ToolIR<'a>>>,
    pub tool_choice: Option<ToolChoiceIR<'a>>,
    pub response_format: Option<ResponseFormatIR>,
    pub cache_control: Option<CacheControlIR>,
    pub metadata: MetadataIR<'a>,
    pub extensions: Option<HashMap<Cow<'a, str>, Box<RawValue>>>,
    pub untouched: UntouchedFlags,
}

impl<'a> Default for RequestIR<'a> {
    fn default() -> Self {
        Self {
            model: Cow::Borrowed(""),
            messages: Vec::new(),
            stream: false,
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            stop: None,
            seed: None,
            tools: None,
            tool_choice: None,
            response_format: None,
            cache_control: None,
            metadata: MetadataIR::default(),
            extensions: None,
            untouched: UntouchedFlags::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum MessageIR<'a> {
    System {
        content: ContentIR<'a>,
    },
    User {
        content: ContentIR<'a>,
    },
    Assistant {
        content: ContentIR<'a>,
        tool_calls: Option<Vec<ToolCallIR<'a>>>,
    },
    Tool {
        tool_call_id: Cow<'a, str>,
        content: ContentIR<'a>,
    },
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum ContentIR<'a> {
    Text(Cow<'a, str>),
    Parts(SmallVec<[ContentPart<'a>; 4]>),
}

#[derive(Clone, Debug)]
pub enum ContentPart<'a> {
    Text(Cow<'a, str>),
    Image { source: ImageSource<'a> },
    Audio { source: MediaSource<'a> },
    File { source: MediaSource<'a> },
}

#[derive(Clone, Debug)]
pub enum ImageSource<'a> {
    Url(Cow<'a, str>),
    B64 {
        media_type: Cow<'a, str>,
        data: Cow<'a, str>,
    },
}

#[derive(Clone, Debug)]
pub enum MediaSource<'a> {
    Url(Cow<'a, str>),
    B64 {
        media_type: Cow<'a, str>,
        data: Cow<'a, str>,
    },
}

#[derive(Clone, Debug)]
pub struct ToolIR<'a> {
    pub name: Cow<'a, str>,
    pub description: Option<Cow<'a, str>>,
    pub parameters_jsonschema: Cow<'a, RawValue>,
}

#[derive(Clone, Debug)]
pub struct ToolCallIR<'a> {
    pub id: Cow<'a, str>,
    pub name: Cow<'a, str>,
    pub arguments_json: Cow<'a, RawValue>,
}

#[derive(Clone, Debug)]
pub enum ToolChoiceIR<'a> {
    Auto,
    None,
    Required,
    Function { name: Cow<'a, str> },
}

#[derive(Clone, Debug)]
pub enum ResponseFormatIR {
    Text,
    JsonObject,
    JsonSchema(Box<RawValue>),
}

#[derive(Clone, Debug)]
pub struct CacheControlIR {
    pub kind: Cow<'static, str>,
}

#[derive(Clone, Debug, Default)]
pub struct MetadataIR<'a> {
    pub openai_logit_bias: Option<HashMap<Cow<'a, str>, f32>>,
    pub openai_logprobs: Option<bool>,
    pub anthropic_thinking: Option<AnthropicThinking<'a>>,
    pub gemini_safety_settings: Option<Vec<GeminiSafetySetting<'a>>>,
}

#[derive(Clone, Debug)]
pub struct AnthropicThinking<'a> {
    pub kind: Cow<'a, str>,
    pub budget_tokens: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct GeminiSafetySetting<'a> {
    pub category: Cow<'a, str>,
    pub threshold: Cow<'a, str>,
}

#[derive(Clone, Debug)]
pub struct ResponseIR<'a> {
    pub id: Cow<'a, str>,
    pub model: Cow<'a, str>,
    pub messages: Vec<MessageIR<'a>>,
    pub finish_reason: Option<FinishReasonIR>,
    pub usage: Option<UsageIR>,
    pub metadata: MetadataIR<'a>,
}

impl<'a> Default for ResponseIR<'a> {
    fn default() -> Self {
        Self {
            id: Cow::Borrowed(""),
            model: Cow::Borrowed(""),
            messages: Vec::new(),
            finish_reason: None,
            usage: None,
            metadata: MetadataIR::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReasonIR {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct UsageIR {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
}

#[derive(Clone, Debug)]
pub enum StreamEventIR<'a> {
    Start {
        request_id: Cow<'a, str>,
    },
    Delta {
        text: Option<Cow<'a, str>>,
        tool_call: Option<ToolCallIR<'a>>,
        thinking: Option<Cow<'a, str>>,
    },
    Usage(UsageIR),
    End {
        finish_reason: FinishReasonIR,
    },
    Error(ErrorIR<'a>),
}

/// 收集业务层对 IR 的改动（max_tokens cap、forced_params 等），用于同协议透传判定空 mutation。
#[derive(Clone, Debug, Default)]
pub struct Mutations {
    pub fields: u32,
}
impl Mutations {
    pub const NONE: Self = Self { fields: 0 };
    pub fn is_empty(&self) -> bool {
        self.fields == 0
    }
    pub fn set_max_tokens(&mut self) {
        self.fields |= 1 << 0;
    }
    pub fn set_temperature(&mut self) {
        self.fields |= 1 << 1;
    }
    pub fn set_metadata(&mut self) {
        self.fields |= 1 << 2;
    }
    pub fn set_tools(&mut self) {
        self.fields |= 1 << 3;
    }
}
