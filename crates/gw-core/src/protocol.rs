use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolKind {
    OpenAi,
    Anthropic,
    Gemini,
}

impl ProtocolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }
}

/// `ApiFormat` is a v1 alias for `ProtocolKind` per integration spec §2.3
/// Bm5 (three buckets: openai / anthropic / gemini). Future refinement
/// (e.g. `openai_chat`, `openai_responses`, `openai_cli`) may shadow this
/// alias with a distinct enum.
pub type ApiFormat = ProtocolKind;

/// Finer-grained shape detection: which API surface within a protocol the
/// client used. Used by capability resolution (T5-D) to gate `OpenAiResponses`
/// vs `OpenAiChat` and similar shape-specific capabilities.
///
/// Distinct from [`ProtocolKind`]: ProtocolKind is the vendor family;
/// ApiShape is the concrete request/response wire shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiShape {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerate,
}
