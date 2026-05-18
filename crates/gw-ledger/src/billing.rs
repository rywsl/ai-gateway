//! Billing record schema — ADR-026 (simple_log).
//!
//! Emitted by `gw-proxy` at request completion and consumed by the ledger
//! flusher to decrement balances and append to the `billing:records` Redis
//! stream.

use gw_core::ids::{ApiKeyId, KeyId, ModelName, UserId, VendorId};
use serde::Serialize;

/// One billing event per (sub-)request. All monetary fields are micro-units
/// (1e-6 of base currency) to keep arithmetic in `i64`.
#[derive(Debug, Serialize, Clone)]
pub struct BillingRecord {
    /// Unix milliseconds (UTC).
    pub ts_ms: u64,
    pub request_id: String,
    pub trace_id: String,
    pub user_id: UserId,
    pub child_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,
    pub client_protocol: String,
    pub upstream_protocol: String,
    pub model: ModelName,
    pub vendor_id: VendorId,
    pub vendor_key_id: KeyId,
    pub status: BillingStatus,
    pub error_code: Option<String>,
    pub http_status: u16,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
    pub cost_micro: u64,
    pub freeze_amount_micro: u64,
    pub markup_product: f32,
    pub usage_source: UsageSource,
    pub retry_count: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub is_stream: bool,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum BillingStatus {
    Ok,
    #[serde(rename = "err")]
    Error,
    Cancelled,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Upstream,
    Estimated,
    FallbackConservative,
}
