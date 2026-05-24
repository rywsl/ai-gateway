//! 6-dim usage extraction with per-dimension provenance.
//!
//! Authoritative spec: docs/superpowers/specs/2026-05-24-billing-robustness-design.md §5.2

use gw_core::usage::UsageSnapshot;

/// Per-dimension confidence level for a single usage value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceTag {
    /// Vendor sent terminal usage event/payload.
    UpstreamFinal,
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

/// Result of one extraction pass over a `ResponseIR` (non-stream) or
/// `StreamEventIR` (one SSE chunk).
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtractedUsage {
    pub values: UsageSnapshot,
    pub provenance: PerDimProvenance,
    /// `true` when this extraction observed the terminal usage event/payload —
    /// triggers `VendorExposure: Streaming → Settled` (see spec §5.1).
    pub is_terminal: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PerDimProvenance {
    pub input: ProvenanceTag,
    pub output: ProvenanceTag,
    pub cache_read: ProvenanceTag,
    pub cache_creation: ProvenanceTag,
    pub thinking: ProvenanceTag,
    pub multimodal_input: ProvenanceTag,
}

/// Codec-specific usage extractor. Implementations live in
/// `crates/gw-codec/src/{openai,anthropic,gemini}/usage_extractor.rs`.
pub trait UsageExtractor: Send + Sync {
    /// Extract usage from a non-stream `ResponseIR`-equivalent JSON document.
    fn extract_response(&self, body: &[u8]) -> ExtractedUsage;

    /// Extract usage from a single SSE-framed stream chunk. Returns `None`
    /// when the chunk carries no usage info (e.g. text-only delta).
    fn extract_chunk(&self, event_bytes: &[u8]) -> Option<ExtractedUsage>;
}
