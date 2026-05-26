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
/// ApiShape is the concrete request/response wire shape. The 4 CLI variants
/// share the same filter chain but bypass IR parse/render (body passthrough).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiShape {
    // Standard API surfaces
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerate,
    // CLI variants (body passthrough, no IR conversion)
    ClaudeCli,
    OpenAiCli,
    OpenAiCompact,
    GeminiCli,
}

impl ApiShape {
    /// Returns the Aether-compatible FORMAT_ID string.
    pub fn format_id(&self) -> &'static str {
        match self {
            Self::OpenAiChat => "openai:chat",
            Self::OpenAiResponses => "openai:responses",
            Self::AnthropicMessages => "anthropic:messages",
            Self::GeminiGenerate => "gemini:generate",
            Self::ClaudeCli => "claude:cli",
            Self::OpenAiCli => "openai:cli",
            Self::OpenAiCompact => "openai:compact",
            Self::GeminiCli => "gemini:cli",
        }
    }

    /// Whether this shape is a CLI variant (body passthrough, no IR).
    pub fn is_cli(&self) -> bool {
        matches!(
            self,
            Self::ClaudeCli | Self::OpenAiCli | Self::OpenAiCompact | Self::GeminiCli
        )
    }

    /// Map to the coarse-grained vendor family.
    pub fn protocol_kind(&self) -> ProtocolKind {
        match self {
            Self::OpenAiChat | Self::OpenAiResponses | Self::OpenAiCli | Self::OpenAiCompact => {
                ProtocolKind::OpenAi
            }
            Self::AnthropicMessages | Self::ClaudeCli => ProtocolKind::Anthropic,
            Self::GeminiGenerate | Self::GeminiCli => ProtocolKind::Gemini,
        }
    }
}
