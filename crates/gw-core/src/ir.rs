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

impl Default for RequestIR<'_> {
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

/// Anthropic 风格 tool_use 块（wire 端 content array 内的 ToolUse 块，与 OpenAI 扁平 tool_calls 一一对应）。
#[derive(Clone, Debug)]
pub struct ToolUseBlockIR<'a> {
    pub id: Cow<'a, str>,
    pub name: Cow<'a, str>,
    pub input: Cow<'a, RawValue>,
}

/// 响应侧 thinking 块集合（Anthropic 与 OpenAI Responses 共用）。
#[derive(Clone, Debug, Default)]
pub struct ResponseThinkingIR<'a> {
    pub blocks: Vec<ThinkingBlockIR<'a>>,
}

#[derive(Clone, Debug)]
pub struct ThinkingBlockIR<'a> {
    pub text: Cow<'a, str>,
    pub signature: Option<Cow<'a, str>>,
}

/// OpenAI Responses 风格 reasoning 摘要块。
#[derive(Clone, Debug, Default)]
pub struct ReasoningBlockIR<'a> {
    pub summary: Cow<'a, str>,
}

#[derive(Clone, Debug)]
pub struct ResponseIR<'a> {
    pub id: Cow<'a, str>,
    pub model: Cow<'a, str>,
    pub messages: Vec<MessageIR<'a>>,
    pub finish_reason: Option<FinishReasonIR>,
    pub usage: Option<UsageIR>,
    pub metadata: MetadataIR<'a>,
    pub thinking: Option<ResponseThinkingIR<'a>>,
    pub reasoning: Option<ReasoningBlockIR<'a>>,
    pub tool_use_blocks: Option<Vec<ToolUseBlockIR<'a>>>,
}

impl Default for ResponseIR<'_> {
    fn default() -> Self {
        Self {
            id: Cow::Borrowed(""),
            model: Cow::Borrowed(""),
            messages: Vec::new(),
            finish_reason: None,
            usage: None,
            metadata: MetadataIR::default(),
            thinking: None,
            reasoning: None,
            tool_use_blocks: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReasonIR {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    /// 载荷为 vendor 原始字符串（如 "pause_turn", "RECITATION"）；
    /// 跨协议 re-render 时由 cross_render::map_finish_reason 决定降级 + lossy log。
    Other(Cow<'static, str>),
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct UsageIR {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
    /// OpenAI Responses completion_tokens_details.reasoning_tokens; Anthropic 无对等，保持 None。
    pub reasoning_tokens: Option<u32>,
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
    /// Anthropic / OpenAI Responses 流式 thinking 块的逐 chunk delta。
    ThinkingDelta {
        text: Cow<'a, str>,
        signature_chunk: Option<Cow<'a, str>>,
    },
    /// 新 tool_call 开启（index 在响应中此 tool_call 的位置）。
    ToolCallStart {
        index: u32,
        id: Cow<'a, str>,
        name: Cow<'a, str>,
    },
    /// 已开启 tool_call 的 arguments JSON 片段追加（累积到完整 JSON 后由 codec 决定何时 emit）。
    ToolCallDelta {
        index: u32,
        arguments_chunk: Cow<'a, str>,
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
    pub fn set_system_prompt(&mut self) {
        self.fields |= 1 << 4;
    }
    pub fn set_messages(&mut self) {
        self.fields |= 1 << 5;
    }
    pub fn set_thinking_config(&mut self) {
        self.fields |= 1 << 6;
    }
    pub fn set_response_format(&mut self) {
        self.fields |= 1 << 7;
    }
    pub fn set_stream(&mut self) {
        self.fields |= 1 << 8;
    }
}

/// Tracks which IR fields were overridden by vendor-key `forced_params`.
/// Parallel to [`Mutations`] but exclusively for forced-params origin tracking;
/// powers the `gw_forced_params_applied_total{field}` metric.
#[derive(Clone, Debug, Default)]
pub struct ForcedMutations {
    pub fields: u32,
}
impl ForcedMutations {
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
    pub fn set_system_prompt(&mut self) {
        self.fields |= 1 << 4;
    }
    pub fn set_messages(&mut self) {
        self.fields |= 1 << 5;
    }
    pub fn set_thinking_config(&mut self) {
        self.fields |= 1 << 6;
    }
    pub fn set_response_format(&mut self) {
        self.fields |= 1 << 7;
    }
    pub fn set_stream(&mut self) {
        self.fields |= 1 << 8;
    }
    pub fn set_top_p(&mut self) {
        self.fields |= 1 << 9;
    }

    /// Returns the field names that were overridden, for metric label emission.
    pub fn field_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        const NAMES: &[(u32, &str)] = &[
            (1 << 0, "max_tokens"),
            (1 << 1, "temperature"),
            (1 << 2, "metadata"),
            (1 << 3, "tools"),
            (1 << 4, "system_prompt"),
            (1 << 5, "messages"),
            (1 << 6, "thinking_config"),
            (1 << 7, "response_format"),
            (1 << 8, "stream"),
            (1 << 9, "top_p"),
        ];
        NAMES
            .iter()
            .filter(move |(bit, _)| self.fields & bit != 0)
            .map(|(_, name)| *name)
    }
}

#[cfg(test)]
mod mutations_bits_tests {
    use super::Mutations;

    #[test]
    fn empty_by_default() {
        let m = Mutations::default();
        assert!(m.is_empty());
        assert_eq!(m.fields, 0);
    }

    #[test]
    fn each_setter_uses_a_distinct_bit() {
        let mut bits = std::collections::HashSet::new();
        for (name, setter) in setters() {
            let mut m = Mutations::default();
            setter(&mut m);
            assert!(!m.is_empty(), "{name} did not flip any bit");
            assert!(
                m.fields.count_ones() == 1,
                "{name} flipped more than one bit"
            );
            assert!(
                bits.insert(m.fields),
                "{name} flipped a bit another setter already owns"
            );
        }
    }

    #[test]
    fn setters_are_idempotent() {
        let mut m = Mutations::default();
        m.set_max_tokens();
        m.set_max_tokens();
        assert_eq!(m.fields.count_ones(), 1);
    }

    #[allow(clippy::type_complexity)]
    fn setters() -> Vec<(&'static str, fn(&mut Mutations))> {
        vec![
            ("max_tokens", Mutations::set_max_tokens),
            ("temperature", Mutations::set_temperature),
            ("metadata", Mutations::set_metadata),
            ("tools", Mutations::set_tools),
            ("system_prompt", Mutations::set_system_prompt),
            ("messages", Mutations::set_messages),
            ("thinking_config", Mutations::set_thinking_config),
            ("response_format", Mutations::set_response_format),
            ("stream", Mutations::set_stream),
        ]
    }
}

#[cfg(test)]
mod forced_mutations_bits_tests {
    use super::ForcedMutations;

    #[test]
    fn empty_by_default() {
        let m = ForcedMutations::default();
        assert!(m.is_empty());
        assert_eq!(m.fields, 0);
    }

    #[test]
    fn none_const_is_empty() {
        assert!(ForcedMutations::NONE.is_empty());
    }

    #[test]
    fn each_setter_uses_a_distinct_bit() {
        let mut bits = std::collections::HashSet::new();
        for (name, setter) in setters() {
            let mut m = ForcedMutations::default();
            setter(&mut m);
            assert!(!m.is_empty(), "{name} did not flip any bit");
            assert!(
                m.fields.count_ones() == 1,
                "{name} flipped more than one bit"
            );
            assert!(
                bits.insert(m.fields),
                "{name} flipped a bit another setter already owns"
            );
        }
    }

    #[test]
    fn setters_are_idempotent() {
        let mut m = ForcedMutations::default();
        m.set_temperature();
        m.set_temperature();
        assert_eq!(m.fields.count_ones(), 1);
    }

    #[test]
    fn field_names_returns_set_fields() {
        let mut m = ForcedMutations::default();
        m.set_temperature();
        m.set_top_p();
        let names: Vec<_> = m.field_names().collect();
        assert_eq!(names, vec!["temperature", "top_p"]);
    }

    #[test]
    fn field_names_empty_when_none() {
        let m = ForcedMutations::NONE;
        assert_eq!(m.field_names().count(), 0);
    }

    #[allow(clippy::type_complexity)]
    fn setters() -> Vec<(&'static str, fn(&mut ForcedMutations))> {
        vec![
            ("max_tokens", ForcedMutations::set_max_tokens),
            ("temperature", ForcedMutations::set_temperature),
            ("metadata", ForcedMutations::set_metadata),
            ("tools", ForcedMutations::set_tools),
            ("system_prompt", ForcedMutations::set_system_prompt),
            ("messages", ForcedMutations::set_messages),
            ("thinking_config", ForcedMutations::set_thinking_config),
            ("response_format", ForcedMutations::set_response_format),
            ("stream", ForcedMutations::set_stream),
            ("top_p", ForcedMutations::set_top_p),
        ]
    }
}

use std::any::Any;

/// Codec 内部的 SSE 累积器状态。filter 持有 `Box<dyn StreamState>` 但不窥视字段。
/// 每个 codec 在 `ProtocolCodec::new_stream_state()` 中返回自家具体类型的 Box。
pub trait StreamState: Send + Sync + Any {
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[cfg(test)]
mod ir_v2_tests {
    use super::*;

    #[test]
    fn response_ir_extension_fields_default_none() {
        let r = ResponseIR::default();
        assert!(r.thinking.is_none());
        assert!(r.reasoning.is_none());
        assert!(r.tool_use_blocks.is_none());
    }

    #[test]
    fn finish_reason_other_carries_payload() {
        let r = FinishReasonIR::Other(Cow::Borrowed("pause_turn"));
        match r {
            FinishReasonIR::Other(s) => assert_eq!(s, "pause_turn"),
            _ => panic!("expected Other"),
        }
    }

    #[test]
    fn usage_ir_default_zero_with_optional_reasoning() {
        let u = UsageIR::default();
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert!(u.cached_tokens.is_none());
        assert!(u.reasoning_tokens.is_none());
    }

    #[test]
    fn stream_event_ir_new_variants_compile() {
        let _ = StreamEventIR::ThinkingDelta {
            text: Cow::Borrowed("thinking..."),
            signature_chunk: None,
        };
        let _ = StreamEventIR::ToolCallStart {
            index: 0,
            id: Cow::Borrowed("call_1"),
            name: Cow::Borrowed("get_weather"),
        };
        let _ = StreamEventIR::ToolCallDelta {
            index: 0,
            arguments_chunk: Cow::Borrowed("{\"loc"),
        };
    }
}
