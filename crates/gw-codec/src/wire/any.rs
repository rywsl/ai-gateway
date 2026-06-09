//! WireAnyRequest — one variant per supported protocol.
//!
//! Adapter `patch_request` (A-T0) operates on this enum.

use crate::anthropic::wire::AnthropicRequest;
use crate::gemini::wire::GeminiRequest;
use crate::kiro::EventStreamFrame;
use crate::openai::wire::OpenAiRequest;
use gw_core::protocol::ProtocolKind;

/// Wire-level request, tagged by protocol.
pub enum WireAnyRequest<'a> {
    OpenAi(Box<OpenAiRequest<'a>>),
    Anthropic(AnthropicRequest<'a>),
    Gemini(GeminiRequest<'a>),
    Kiro(EventStreamFrame),
}

impl WireAnyRequest<'_> {
    pub fn kind(&self) -> ProtocolKind {
        match self {
            Self::OpenAi(_) => ProtocolKind::OpenAi,
            Self::Anthropic(_) => ProtocolKind::Anthropic,
            Self::Gemini(_) => ProtocolKind::Gemini,
            Self::Kiro(_) => ProtocolKind::Kiro,
        }
    }
}
