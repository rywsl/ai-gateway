//! 6-dim usage extraction with per-dimension provenance.
//!
//! Authoritative spec: docs/spec/06-billing-ledger.md

use gw_core::usage::UsageSnapshot;

/// Re-exported from `gw_core::usage` so callers in both `gw-codec` and
/// `gw-ledger` import from a single canonical location.
pub use gw_core::usage::ProvenanceTag;

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
