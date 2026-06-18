//! Token-usage snapshot — POD shared between codec extractors and the
//! UsageRecord that the ledger flusher publishes to `usage:records`.
//!
//! Authoritative location per integration spec §3 C-7. Six dimensions cover
//! all current codecs (OpenAI / Anthropic / Gemini / Codex Responses /
//! Kiro EventStream): plain input, output, cache-read, cache-creation,
//! thinking (extended), and multimodal-derived input tokens.

use serde::{Deserialize, Serialize};

/// `CURRENT_PV` is the protocol version emitted by new `UsageSnapshot` records.
/// Bump in lockstep with Go `shared/rustpact/version.go::RustpactCurrentPV`.
/// See WS-8 REV-FINAL-H8.1 / H8.2 and v2.1 §11 M10.
pub const CURRENT_PV: u32 = 2;

/// Serde default for legacy records that predate `_pv`: they are treated as
/// protocol version 1 (the pre-WS-8 schema with only 6 token dimensions).
/// This is distinct from the `Default` trait impl, which sets `pv = CURRENT_PV`
/// for in-code construction of new records.
fn default_pv() -> u32 {
    1
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
    pub thinking_tokens: Option<u32>,
    pub multimodal_input_tokens: Option<u32>,

    // --- C1 + Codec-2 + Codec-5: audio, prediction, cache-creation tier, server-tool counts.
    // Option<i64> mirrors Go `*int64`: a billed-but-zero call is `Some(0)`, a
    // non-applicable dimension (e.g. non-audio model) is `None`. Skip-serializing
    // on None keeps legacy JSON readers (and Go omitempty) byte-stable. ---
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_prediction_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_prediction_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_5m_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_1h_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tool_web_search_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tool_web_fetch_count: Option<i64>,

    // --- C11: cross-process protocol version. Older records default to 1 via
    // `default_pv`; new emit writes `CURRENT_PV` (= 2). Renamed to `_pv` on the
    // wire so consumers can disambiguate schema generations. ---
    #[serde(rename = "_pv", default = "default_pv")]
    pub pv: u32,
}

impl Default for UsageSnapshot {
    fn default() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            cache_read_input_tokens: None,
            cache_creation_input_tokens: None,
            thinking_tokens: None,
            multimodal_input_tokens: None,
            audio_input_tokens: None,
            audio_output_tokens: None,
            accepted_prediction_tokens: None,
            rejected_prediction_tokens: None,
            cache_creation_5m_tokens: None,
            cache_creation_1h_tokens: None,
            server_tool_web_search_count: None,
            server_tool_web_fetch_count: None,
            pv: CURRENT_PV,
        }
    }
}

/// Per-dimension confidence level for a single usage value.
/// Authoritative location: spec §5.2.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceTag {
    /// Vendor sent terminal usage event/payload.
    UpstreamFinal,
    /// Parsed from an upstream CLI response body or stream event.
    CliExtracted,
    /// Parsed mid-stream; never saw final.
    UpstreamPartial,
    /// Fell back to tokenizer/estimate.
    Estimated,
    /// InflightLedger reconciler-emitted.
    CrashFallback,
    /// Upstream produced nothing or context is half-residual.
    #[default]
    Unknown,
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
        // M10/C11: Default construction writes the current protocol version so
        // new emit is tagged _pv: 2; legacy records (no _pv key) deserialize to 1.
        assert_eq!(u.pv, CURRENT_PV);
        assert!(u.audio_input_tokens.is_none());
        assert!(u.server_tool_web_fetch_count.is_none());
    }

    #[test]
    fn serde_legacy_record_without_pv_defaults_to_one() {
        let s = r#"{"input_tokens":10,"output_tokens":5}"#;
        let u: UsageSnapshot = serde_json::from_str(s).unwrap();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.pv, 1, "legacy records without _pv must deserialize as pv=1");
    }

    #[test]
    fn serde_pv2_with_audio_roundtrip() {
        let u = UsageSnapshot {
            input_tokens: 100,
            output_tokens: 50,
            audio_input_tokens: Some(3),
            audio_output_tokens: Some(0),
            pv: CURRENT_PV,
            ..Default::default()
        };
        let s = serde_json::to_string(&u).unwrap();
        assert!(s.contains("\"_pv\":2"), "serialized JSON must tag _pv=2: {s}");
        assert!(s.contains("\"audio_input_tokens\":3"));
        // Some(0) is still emitted (not skipped) — billed-but-zero must survive.
        assert!(s.contains("\"audio_output_tokens\":0"));
        let back: UsageSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(u, back);
        assert_eq!(back.pv, CURRENT_PV);
        assert_eq!(back.audio_input_tokens, Some(3));
        assert_eq!(back.audio_output_tokens, Some(0));
    }

    #[test]
    fn serde_unknown_future_field_is_ignored() {
        // Forward-compat: a future Rust may emit fields this build doesn't know.
        // Decoding must NOT reject them (no deny_unknown_fields — see WS-8 H8.1).
        let s = r#"{"input_tokens":1,"output_tokens":0,"_pv":2,"some_future_field":"x"}"#;
        let u: UsageSnapshot = serde_json::from_str(s).unwrap();
        assert_eq!(u.input_tokens, 1);
        assert_eq!(u.pv, 2);
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

    #[test]
    fn provenance_tag_cli_extracted_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&ProvenanceTag::CliExtracted).unwrap(),
            "\"cli_extracted\""
        );
    }
}
