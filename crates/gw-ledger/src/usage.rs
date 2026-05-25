//! UsageRecord — emitted by `gw-proxy` at request completion and consumed by
//! the ledger flusher to XADD `usage:records` (ADR-Ca-1 + ADR-Ca-3).
//!
//! Cost computation is NOT in Rust. The Go side reads `usage:records` and
//! produces the final cost record on `settled_billing`. Rust ships the
//! 6-dim UsageSnapshot, the 5-field MarkupChainSnapshot, the freeze
//! amount it actually reserved, and the EstimatePricing version it used.

use gw_core::ids::{ApiKeyId, KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::{ProvenanceTag, UsageSnapshot};
use serde::{Deserialize, Serialize};

/// How far the vendor exposure progressed before the request ended.
/// Used by Go to decide whether to bill (Settled) or reconcile (Terminated /
/// CrashFallback). Replaces the old `UsageSource` field (W4-T1).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VendorExposureState {
    /// Upstream sent the final usage event; stream/response fully consumed.
    #[default]
    Settled,
    /// Upstream received the request but connection was cut before a final
    /// usage event — Go must reconcile against the inflight hash.
    Terminated,
    /// InflightLedger reconciler produced the usage record on crash recovery.
    CrashFallback,
}

/// Per-dimension provenance snapshot frozen at UsageRecord emission time.
/// Each field records the highest-confidence `ProvenanceTag` seen across all
/// extraction passes for that usage dimension.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct UsageProvenance {
    pub input: ProvenanceTag,
    pub output: ProvenanceTag,
    pub cache_read: ProvenanceTag,
    pub cache_creation: ProvenanceTag,
    pub thinking: ProvenanceTag,
    pub multimodal_input: ProvenanceTag,
}

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

    // --- Usage (6-dim) + provenance ---
    pub usage: UsageSnapshot,

    // --- Attempt / exposure (W4-T1) ---
    /// 0-based index of the attempt that produced this record. Matches
    /// `inflight:{instance}:{request_id}:{attempt_index}` in Redis.
    pub attempt_index: u32,
    /// How far the vendor exposure progressed; determines Go reconciliation
    /// path (bill / reconcile / crash-recover).
    pub vendor_exposure_state: VendorExposureState,
    /// Per-dimension confidence level frozen at emission time.
    pub usage_provenance: UsageProvenance,

    // --- Timing ---
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub is_stream: bool,

    // --- Freeze snapshot (for Go reconciliation) ---
    pub freeze_amount_micro: u64,
    /// EstimatePricing version Rust used at freeze time. Currently writes 0;
    /// C-b will source the real version from the `entity:estimate_pricing`
    /// snapshot.
    pub estimate_pricing_version: u64,

    // --- Markup snapshot (Go re-computes cost) ---
    pub markup_chain_snapshot: MarkupChainSnapshot,

    // --- Long-running operation (G5 / go-onboarding-gaps T0) ---
    /// Gemini `:predictLongRunning` / OpenAI `/v1/videos` operation name
    /// (`"operations/..."`) extracted by `codec.parse_response`. `None` for
    /// all ordinary (non-LRO) requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    Ok,
    #[serde(rename = "err")]
    Error,
    Cancelled,
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
            attempt_index: 0,
            vendor_exposure_state: VendorExposureState::Settled,
            usage_provenance: UsageProvenance::default(),
            latency_ms: 250,
            ttft_ms: Some(80),
            is_stream: true,
            freeze_amount_micro: 1500,
            estimate_pricing_version: 0,
            markup_chain_snapshot: MarkupChainSnapshot::identity(),
            operation_id: None,
        };
        let s = serde_json::to_string(&rec).unwrap();
        // Smoke: critical fields appear; the retired fields are GONE.
        assert!(s.contains("\"usage\""));
        assert!(s.contains("\"input_tokens\":100"));
        assert!(s.contains("\"markup_chain_snapshot\""));
        assert!(s.contains("\"estimate_pricing_version\":0"));
        assert!(!s.contains("cost_micro"), "Rust must not emit cost_micro");
        assert!(
            !s.contains("\"usage_source\""),
            "usage_source must be removed"
        );
        assert!(
            !s.contains("\"retry_count\""),
            "retry_count must be removed"
        );
        assert!(s.contains("\"attempt_index\":0"));
        assert!(s.contains("\"vendor_exposure_state\":\"settled\""));
        assert!(s.contains("\"usage_provenance\""));
        // operation_id: None must be omitted via skip_serializing_if.
        assert!(
            !s.contains("operation_id"),
            "operation_id must be omitted when None"
        );

        // operation_id: Some serialises as the string value.
        let rec_with_op = UsageRecord {
            operation_id: Some("operations/xyz-123".into()),
            ..rec
        };
        let s2 = serde_json::to_string(&rec_with_op).unwrap();
        assert!(
            s2.contains("\"operation_id\":\"operations/xyz-123\""),
            "operation_id must appear when Some"
        );
    }
}
