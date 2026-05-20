//! WireAny enums — one variant per supported protocol.
//!
//! Adapter `patch_request` / `patch_response` (A-T0) and Phase 2's
//! semantic-trait dispatch (`apply_*_any`) operate on these enums.
//!
//! Phase 1d note: `WireAnyResponse` and `WireAnyStreamChunk` are empty
//! enums with a `PhantomData` carrier. The wire-level response/stream
//! structs they will hold do not exist yet; Phase 2 (codec S2 / S4)
//! introduces them and replaces the `_Phantom` variant with the real
//! OpenAi/Anthropic/Gemini ones.

use crate::anthropic::wire::AnthropicRequest;
use crate::gemini::wire::GeminiRequest;
use crate::openai::wire::OpenAiRequest;
use gw_core::protocol::ProtocolKind;
use std::marker::PhantomData;

/// Wire-level request, tagged by protocol.
pub enum WireAnyRequest<'a> {
    OpenAi(OpenAiRequest<'a>),
    Anthropic(AnthropicRequest<'a>),
    Gemini(GeminiRequest<'a>),
}

impl WireAnyRequest<'_> {
    pub fn kind(&self) -> ProtocolKind {
        match self {
            Self::OpenAi(_) => ProtocolKind::OpenAi,
            Self::Anthropic(_) => ProtocolKind::Anthropic,
            Self::Gemini(_) => ProtocolKind::Gemini,
        }
    }
}

/// Wire-level response, tagged by protocol.
///
/// Phase 1d: uninhabited. Phase 2 (codec S4) introduces wire-level response
/// structs and replaces `_Phantom` with `OpenAi(OpenAiResponse<'a>)`,
/// `Anthropic(AnthropicResponse<'a>)`, `Gemini(GeminiResponse<'a>)`.
pub enum WireAnyResponse<'a> {
    #[doc(hidden)]
    _Phantom(PhantomData<&'a ()>),
}

impl WireAnyResponse<'_> {
    pub fn kind(&self) -> ProtocolKind {
        match self {
            WireAnyResponse::_Phantom(_) => unreachable!(
                "WireAnyResponse::_Phantom is uninhabited; Phase 2 fills in real variants"
            ),
        }
    }
}

/// Wire-level stream chunk, tagged by protocol.
///
/// Phase 1d: uninhabited. Phase 2 (codec S3) introduces wire-level stream
/// chunk structs and replaces `_Phantom` with the real variants.
pub enum WireAnyStreamChunk<'a> {
    #[doc(hidden)]
    _Phantom(PhantomData<&'a ()>),
}

impl WireAnyStreamChunk<'_> {
    pub fn kind(&self) -> ProtocolKind {
        match self {
            WireAnyStreamChunk::_Phantom(_) => unreachable!(
                "WireAnyStreamChunk::_Phantom is uninhabited; Phase 2 fills in real variants"
            ),
        }
    }
}
