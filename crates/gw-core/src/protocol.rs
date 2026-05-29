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

/// Resolve the protocol format spoken to the upstream for a given vendor key.
///
/// This is the **single source of truth** for client→upstream codec selection.
/// It MUST be used identically at the routing health gate (the *read* side —
/// `is_available(key, fmt)`) and at codec/health recording (the *write* side —
/// `record(key, fmt)`); deriving the format independently in those two places
/// is what caused the cross-protocol health-bucket mismatch (audit 2026-05-30
/// §1.5), where a protocol-conversion key was selected under the client bucket
/// but its cooldown/circuit was written to the upstream bucket.
///
/// Selection:
/// 1. client format is in `accessible` → client (same-protocol passthrough);
/// 2. else the first accessible endpoint (deterministic — DB insertion order);
/// 3. else `client` as a *deferred error*: callers re-check
///    `accessible.is_empty()` and emit `gw_no_upstream_codec`. This is NOT a
///    fail-operational fallback — an empty endpoint set always errors at the
///    call site; returning `client` here merely keeps the function total.
pub fn resolve_upstream_format(accessible: &[ApiFormat], client: ApiFormat) -> ApiFormat {
    if accessible.contains(&client) {
        client
    } else if let Some(&first) = accessible.first() {
        first
    } else {
        client
    }
}

#[cfg(test)]
mod resolve_upstream_format_tests {
    use super::*;

    #[test]
    fn same_protocol_passthrough_prefers_client() {
        let acc = [ProtocolKind::Anthropic, ProtocolKind::OpenAi];
        assert_eq!(
            resolve_upstream_format(&acc, ProtocolKind::OpenAi),
            ProtocolKind::OpenAi
        );
    }

    #[test]
    fn cross_protocol_uses_first_accessible() {
        // Client speaks OpenAi but the key only accepts an Anthropic upstream.
        let acc = [ProtocolKind::Anthropic];
        assert_eq!(
            resolve_upstream_format(&acc, ProtocolKind::OpenAi),
            ProtocolKind::Anthropic
        );
    }

    #[test]
    fn empty_accessible_returns_client_deferred_error() {
        // The call site re-checks is_empty() and errors; the value is unused.
        assert_eq!(
            resolve_upstream_format(&[], ProtocolKind::Gemini),
            ProtocolKind::Gemini
        );
    }
}
