//! Per-request context (`ReqCtx`) — the state container threaded through every
//! Pingora filter callback (`new_ctx` → `early_request_filter` → … → `logging`).
//!
//! Mutability rules: callers always hold `&mut ReqCtx`. Filters write the
//! fields they own (e.g. `early_request_filter` writes `client_codec`,
//! `request_filter` writes `user` / `merged_perm` / `freeze_guard`, …).
//!
//! Lifetime: a single `ReqCtx` lives from request acceptance through `logging`.
//! `Drop` of the embedded [`RequestFreeze`] and [`StreamSlot`] enforces the
//! ADR-004 freeze-rollback and ADR-024 stream-slot-release contracts even on
//! cancellation paths.

use bytes::{Bytes, BytesMut};
use gw_auth::ratelimit::StreamSlot;
use gw_codec::AnyCodec;
use gw_core::debug::{DebugMode, TraceBuffer};
use gw_core::entity::{Pricing, UserEntry, Vendor, VendorKey};
use gw_core::ids::{ApiKeyId, KeyId, ModelName, RequestId, TraceId, UserId};
use gw_core::permissions::MergedPermissions;
use gw_core::versioned::Versioned;
use gw_ledger::RequestFreeze;
use gw_routing::in_flight::InFlightGuard;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct DownstreamRequestMeta {
    pub method: String,
    pub url: String,
    pub http_version: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct UpstreamRequestMeta {
    pub method: String,
    pub url: String,
    pub http_version: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ResponseMeta {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

/// Mutable accumulator for token-usage seen on either the unary-response path
/// or in stream `Usage` events. The `logging` filter reads these counters
/// into the `UsageRecord` emitted to the usage stream.
#[derive(Default, Clone, Copy, Debug)]
pub struct UsageAccumulator {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
    pub cache_creation_tokens: Option<u32>,
    pub thinking_tokens: Option<u32>,
    pub multimodal_input_tokens: Option<u32>,
}

#[derive(Default)]
pub struct ReqTiming {
    pub start: Option<Instant>,
    pub ttft: Option<Instant>,
    pub end: Option<Instant>,
    pub sent_ms: Option<i64>,
    pub settled_ms: Option<i64>,
}

pub struct ReqCtx {
    // --- IDs / protocol ---
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub client_codec: Option<AnyCodec>,
    pub upstream_codec: Option<AnyCodec>,

    // --- Auth / permissions ---
    pub user: Option<Arc<Versioned<UserEntry>>>,
    pub api_key_id: Option<ApiKeyId>,
    pub merged_perm: Option<MergedPermissions>,

    // --- Request shape ---
    pub model: Option<ModelName>,
    pub is_stream: bool,
    pub idempotent: bool,
    pub cached_body: Option<Bytes>,
    /// ADR-S3: optional client-supplied conversation identifier read from the
    /// `X-Conversation-Id` request header in `early_request_filter`. Threaded
    /// into `PickCtx.conversation_id` so the `Sticky` strategy can bind a
    /// conversation to a vendor key. `None` when the header is absent —
    /// strategies that consume it fall back to "first available" behaviour.
    pub conversation_id: Option<String>,

    // --- Routing / vendor ---
    pub picked_vendor_key: Option<Arc<VendorKey>>,
    pub picked_vendor: Option<Arc<Versioned<Vendor>>>,
    pub pricing: Option<Arc<Versioned<Pricing>>>,

    // --- Lifecycle / ledger ---
    pub timing: ReqTiming,
    pub usage: UsageAccumulator,
    pub vendor_exposure: crate::billing_lifecycle::VendorExposure,
    pub attempt_index: u32,
    pub freeze_guard: Option<RequestFreeze>,
    pub stream_slot: Option<StreamSlot>,
    pub bytes_sent_to_client: AtomicU64,
    /// RAII handle on the picked vendor key's in-flight counter. Held by the
    /// proxy from `upstream_peer` through end-of-request so the counter
    /// decrement runs on every termination path (success, error, cancel).
    /// `None` if no upstream was picked (e.g. request short-circuited in
    /// `request_filter`).
    pub inflight_guard: Option<InFlightGuard>,

    // --- Streaming buffer (response_body_filter leftovers) ---
    /// Bytes from an upstream chunk that did not contain a full SSE event.
    /// Carried forward to the next callback so we can re-parse at the
    /// `\n\n` boundary.
    pub sse_leftover: Vec<u8>,

    /// Codec-internal SSE accumulator. Set in `upstream_request_filter`
    /// (after the upstream codec is resolved in `rewrite_request`) when the
    /// request is a streaming one; threaded into `parse_stream_chunk` for
    /// every framed event so the codec can buffer partial events (tool-call
    /// argument deltas, thinking signatures, …) across chunk boundaries.
    /// `None` on the non-stream path.
    pub stream_state: Option<Box<dyn gw_core::ir::StreamState>>,

    // --- HTTP status seen on response ---
    pub upstream_status: Option<u16>,
    pub retry_count: u32,
    /// Keys already tried (and failed) for this request, excluded from
    /// candidate selection on cross-key retry (G10). Empty on the first
    /// attempt.
    pub retry_blacklist: Vec<KeyId>,
    /// G10 — gateway error code stamped by `fail_to_connect` when an upstream
    /// connect attempt is *not* going to be retried (either because TLS errors
    /// short-circuit, or because `retry_count` reached `MAX_CONNECT_RETRIES`).
    /// Read by `logging` to surface the right `GW_*` code in metrics / detailed
    /// logs. `None` on the happy path.
    pub upstream_error_code: Option<&'static str>,

    // --- Cross-protocol response re-render accumulator (G8 + G9) ---
    /// Accumulator across events in one chunk; taken+frozen by
    /// `response_body_filter`. MUST stay `BytesMut`: `T4-A` appends multiple
    /// stream-event renders into this per chunk, so a `Bytes` would silently
    /// drop earlier events.
    pub pending_client_body: Option<BytesMut>,

    // --- Error classification (T6-B) ---
    /// Set by the classifier (filters that detect upstream/short-circuit errors)
    /// so the `logging` filter can fan the outcome out to the registered
    /// ErrorListener stack. `None` on the happy path; existing tests are
    /// unaffected.
    pub error_outcome: Option<gw_core::error::ClassifierOutcome>,

    // --- HMAC signature (T6-C body-after) ---
    pub signature_required: bool,
    pub signature_algo: gw_core::entity::SignatureAlgo,
    pub signature_received: Option<Vec<u8>>,
    pub signature_timestamp: Option<String>,
    pub req_content_encoding: gw_codec::compression::Encoding,
    /// G7 — HMAC secret bytes hoisted out of the resolved `ApiKeyEntry` so the
    /// body-verify call site (after the body-read loop in `request_filter`)
    /// can run without keeping the `ApiKey` `Arc` alive. v1.5 reuses the
    /// api-key hash as the HMAC secret; v2 will add a dedicated
    /// `signature_secret: Vec<u8>` field to `ApiKeyEntry` (tracked as a Wave 5
    /// or later follow-up). Empty when `signature_required = false`.
    pub signature_secret: Vec<u8>,

    // --- Management path / capability (G-α.9 / G-β wire) ---
    /// `true` if the request path matched `/admin/*` and is destined for the
    /// management plane rather than the LLM business path.
    pub mgmt: bool,
    /// Capability set resolved for this request (header / model-alias /
    /// retry-upgrade inputs collapsed into a single requirement). Filled by
    /// the request filter once `CapabilityResolver` is wired into
    /// `LlmGateway`; `None` in v1 (see TODO in `filters::request`).
    pub required_caps: Option<gw_capability::types::RequiredCapabilities>,

    // --- Tunnel (ADR-T-2) ---
    /// T6 / ADR-T-2 — set by `pick_peer` to `"direct"` or `Some(node_id)`.
    /// Read by `logging` for the detailed-log field and by metrics labels.
    pub tunnel_resolution: Option<String>,

    // --- Long-running operation (G5 / go-onboarding-gaps T0) ---
    /// G5: Gemini `:predictLongRunning` / OpenAI `/v1/videos` top-level
    /// `name: "operations/..."` extracted by `codec.parse_response`.
    /// `None` for all ordinary (non-LRO) requests.
    pub operation_id: Option<String>,

    // --- Go-onboarding-blockers (B1 non-stream + B2 mgmt token) ---
    /// B1: non-stream upstream body accumulator (EOS triggers codec::parse_response).
    /// `None` on the streaming happy path; allocated lazily by the
    /// `response_body_filter` non-stream branch (T2-B).
    pub upstream_body_accum: Option<bytes::BytesMut>,

    /// B2: management-path verified user (set by the mgmt branch of
    /// `request_filter` after `verify_mgmt_token` succeeds; T3-A/B).
    pub mgmt_user_id: Option<gw_core::ids::UserId>,

    /// B2: management-path verified scope (set alongside `mgmt_user_id`).
    pub mgmt_scope: Option<gw_core::entity::MgmtScope>,

    // --- Detailed archive (wire-fidelity capture) ---
    /// Raw client-facing request body bytes (before any codec render). `None`
    /// until `request_filter` has finished reading the body.
    pub downstream_request_body: Option<Arc<Bytes>>,
    /// Method+URL+headers snapshot for the downstream request. Captured at the
    /// same point as `downstream_request_body`.
    pub downstream_request_meta: Option<DownstreamRequestMeta>,
    /// Final upstream-bound request body bytes. Captured at the end of
    /// `upstream::rewrite_request` (after any codec re-render).
    pub upstream_request_body: Option<Arc<Bytes>>,
    /// Method+URL+headers snapshot for the upstream-bound request.
    pub upstream_request_meta: Option<UpstreamRequestMeta>,
    /// Accumulator for raw upstream response bytes (always-on; the legacy B1
    /// `upstream_body_accum` was streaming-conditional and is now subsumed).
    pub archive_upstream_response_body: bytes::BytesMut,
    /// Accumulator for downstream-bound (post-render) response bytes.
    pub archive_downstream_response_body: bytes::BytesMut,
    /// Upstream response headers + status snapshot, taken in `response_filter`.
    pub archive_upstream_response_meta: Option<ResponseMeta>,
    /// Downstream response headers + status snapshot, taken at EOS in
    /// `response_body_filter` (after any codec header rewrite via response_filter).
    pub archive_downstream_response_meta: Option<ResponseMeta>,

    // --- Debug ---
    pub trace: TraceBuffer,
    pub debug: DebugMode,
}

impl Default for ReqCtx {
    /// Builds a `ReqCtx` with empty IDs. Only intended for tests and
    /// scaffolding assertions; the proxy hot path always uses
    /// [`ReqCtx::new`] with a freshly generated trace/request id.
    fn default() -> Self {
        Self::new(TraceId::from(String::new()), RequestId::from(String::new()))
    }
}

impl ReqCtx {
    pub fn new(trace_id: TraceId, request_id: RequestId) -> Self {
        Self {
            trace_id,
            request_id,
            client_codec: None,
            upstream_codec: None,
            user: None,
            api_key_id: None,
            merged_perm: None,
            model: None,
            is_stream: false,
            idempotent: true,
            cached_body: None,
            conversation_id: None,
            picked_vendor_key: None,
            picked_vendor: None,
            pricing: None,
            timing: ReqTiming::default(),
            usage: UsageAccumulator {
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: None,
                cache_creation_tokens: None,
                thinking_tokens: None,
                multimodal_input_tokens: None,
            },
            vendor_exposure: crate::billing_lifecycle::VendorExposure::Idle,
            attempt_index: 0,
            freeze_guard: None,
            stream_slot: None,
            bytes_sent_to_client: AtomicU64::new(0),
            inflight_guard: None,
            sse_leftover: Vec::new(),
            stream_state: None,
            upstream_status: None,
            retry_count: 0,
            retry_blacklist: Vec::new(),
            upstream_error_code: None,
            pending_client_body: None,
            error_outcome: None,
            signature_required: false,
            signature_algo: gw_core::entity::SignatureAlgo::default(),
            signature_received: None,
            signature_timestamp: None,
            req_content_encoding: gw_codec::compression::Encoding::Identity,
            signature_secret: Vec::new(),
            mgmt: false,
            required_caps: None,
            tunnel_resolution: None,
            operation_id: None,
            upstream_body_accum: None,
            mgmt_user_id: None,
            mgmt_scope: None,
            downstream_request_body: None,
            downstream_request_meta: None,
            upstream_request_body: None,
            upstream_request_meta: None,
            archive_upstream_response_body: bytes::BytesMut::new(),
            archive_downstream_response_body: bytes::BytesMut::new(),
            archive_upstream_response_meta: None,
            archive_downstream_response_meta: None,
            trace: TraceBuffer::new(),
            debug: DebugMode::DISABLED,
        }
    }

    #[inline(always)]
    pub fn phase_start(&mut self, name: &'static str) {
        let alloc = super::mem::sample_thread_alloc();
        self.trace.phase_start(name, alloc);
    }

    #[inline(always)]
    pub fn phase_end(&mut self) {
        let alloc = super::mem::sample_thread_alloc();
        self.trace.phase_end(alloc);
    }

    /// Effective UserId for ledger / rate-limit lookups: merged_perm
    /// (if available) carries the sub-account → main-account remap;
    /// otherwise fall back to the user record's own id.
    pub fn effective_user_id(&self) -> Option<&UserId> {
        if let Some(p) = &self.merged_perm {
            return Some(&p.effective_user_id);
        }
        self.user.as_ref().map(|u| &u.value.user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_accumulator_default_has_six_dims() {
        let u = UsageAccumulator::default();
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert!(u.cached_tokens.is_none());
        assert!(u.cache_creation_tokens.is_none());
        assert!(u.thinking_tokens.is_none());
        assert!(u.multimodal_input_tokens.is_none());
    }
}
