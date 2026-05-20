//! Token-usage snapshot — POD shared between codec extractors and the
//! UsageRecord that the ledger flusher publishes to `usage:records`.
//!
//! Authoritative location per integration spec §3 C-7. Six dimensions cover
//! all current codecs (OpenAI / Anthropic / Gemini / Codex Responses /
//! Kiro EventStream): plain input, output, cache-read, cache-creation,
//! thinking (extended), and multimodal-derived input tokens.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
    pub thinking_tokens: Option<u32>,
    pub multimodal_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_zero_none() {
        let u = UsageSnapshot::default();
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert!(u.cache_read_input_tokens.is_none());
        assert!(u.cache_creation_input_tokens.is_none());
        assert!(u.thinking_tokens.is_none());
        assert!(u.multimodal_input_tokens.is_none());
    }

    #[test]
    fn serde_roundtrip_preserves_optional_none() {
        let u = UsageSnapshot {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };
        let s = serde_json::to_string(&u).unwrap();
        let back: UsageSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(u, back);
    }
}
