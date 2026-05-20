//! UsageRecord — emitted by `gw-proxy` at request completion and consumed by
//! the ledger flusher to XADD `usage:records` (ADR-Ca-1 + ADR-Ca-3).
//!
//! Cost computation is NOT in Rust. The Go side reads `usage:records` and
//! produces the final cost record on `settled_billing`. Rust ships the
//! 6-dim UsageSnapshot, the 5-field MarkupChainSnapshot, the freeze
//! amount it actually reserved, and the EstimatePricing version it used.

use gw_core::ids::{ApiKeyId, KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct UsageRecord {
    // --- Identity / trace ---
    pub ts_ms: u64,
    pub request_id: String,
    pub trace_id: String,

    // --- Subjects ---
    pub user_id: UserId,
    pub child_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,

    // --- Protocol path ---
    pub client_protocol: String,
    pub upstream_protocol: String,
    pub model: ModelName,
    pub vendor_id: VendorId,
    pub vendor_key_id: KeyId,

    // --- Result ---
    pub status: UsageStatus,
    pub error_code: Option<String>,
    pub http_status: u16,

    // --- Usage (6-dim) + source ---
    pub usage: UsageSnapshot,
    pub usage_source: UsageSource,

    // --- Timing ---
    pub retry_count: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub is_stream: bool,

    // --- Freeze snapshot (for Go reconciliation) ---
    pub freeze_amount_micro: u64,
    /// EstimatePricing version Rust used at freeze time. Phase 1b writes 0
    /// (placeholder); Phase 5 (C-b) sources the real version from the
    /// `entity:estimate_pricing` snapshot.
    pub estimate_pricing_version: u64,

    // --- Markup snapshot (Go re-computes cost) ---
    pub markup_chain_snapshot: MarkupChainSnapshot,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    Ok,
    #[serde(rename = "err")]
    Error,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Upstream,
    Estimated,
    FallbackConservative,
    Partial,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_record_json_shape() {
        let rec = UsageRecord {
            ts_ms: 1700000000000,
            request_id: "req-1".into(),
            trace_id: "tr-1".into(),
            user_id: UserId::from("u1"),
            child_id: None,
            api_key_id: None,
            client_protocol: "openai".into(),
            upstream_protocol: "openai".into(),
            model: ModelName::from("gpt-4"),
            vendor_id: VendorId::from("v1"),
            vendor_key_id: KeyId::from("k1"),
            status: UsageStatus::Ok,
            error_code: None,
            http_status: 200,
            usage: UsageSnapshot {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            },
            usage_source: UsageSource::Upstream,
            retry_count: 0,
            latency_ms: 250,
            ttft_ms: Some(80),
            is_stream: true,
            freeze_amount_micro: 1500,
            estimate_pricing_version: 0,
            markup_chain_snapshot: MarkupChainSnapshot::placeholder(),
        };
        let s = serde_json::to_string(&rec).unwrap();
        // Smoke: critical fields appear, cost_micro is GONE.
        assert!(s.contains("\"usage\""));
        assert!(s.contains("\"input_tokens\":100"));
        assert!(s.contains("\"markup_chain_snapshot\""));
        assert!(s.contains("\"estimate_pricing_version\":0"));
        assert!(!s.contains("cost_micro"), "Rust must not emit cost_micro");
    }
}
