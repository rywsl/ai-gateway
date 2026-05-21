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
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

/// Mutable accumulator for token-usage seen on either the unary-response path
/// or in stream `Usage` events. The `logging` filter reads these counters
/// into the `UsageRecord` emitted to the usage stream.
pub struct UsageAccumulator {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: Option<u32>,
}

#[derive(Default)]
pub struct ReqTiming {
    pub start: Option<Instant>,
    pub ttft: Option<Instant>,
    pub end: Option<Instant>,
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

    // --- Routing / vendor ---
    pub picked_vendor_key: Option<Arc<VendorKey>>,
    pub picked_vendor: Option<Arc<Versioned<Vendor>>>,
    pub pricing: Option<Arc<Versioned<Pricing>>>,

    // --- Lifecycle / ledger ---
    pub timing: ReqTiming,
    pub usage: UsageAccumulator,
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

    // --- HTTP status seen on response ---
    pub upstream_status: Option<u16>,
    pub retry_count: u32,
    /// Keys already tried (and failed) for this request, excluded from
    /// candidate selection on cross-key retry (G10). Empty on the first
    /// attempt.
    pub retry_blacklist: Vec<KeyId>,

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

    // --- Management path / capability (G-α.9 / G-β wire) ---
    /// `true` if the request path matched `/admin/*` and is destined for the
    /// management plane rather than the LLM business path.
    pub mgmt: bool,
    /// Capability set resolved for this request (header / model-alias /
    /// retry-upgrade inputs collapsed into a single requirement). Filled by
    /// the request filter once `CapabilityResolver` is wired into
    /// `LlmGateway`; `None` in v1 (see TODO in `filters::request`).
    pub required_caps: Option<gw_capability::types::RequiredCapabilities>,

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
            picked_vendor_key: None,
            picked_vendor: None,
            pricing: None,
            timing: ReqTiming::default(),
            usage: UsageAccumulator {
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: None,
            },
            freeze_guard: None,
            stream_slot: None,
            bytes_sent_to_client: AtomicU64::new(0),
            inflight_guard: None,
            sse_leftover: Vec::new(),
            upstream_status: None,
            retry_count: 0,
            retry_blacklist: Vec::new(),
            pending_client_body: None,
            error_outcome: None,
            signature_required: false,
            signature_algo: gw_core::entity::SignatureAlgo::default(),
            signature_received: None,
            signature_timestamp: None,
            req_content_encoding: gw_codec::compression::Encoding::Identity,
            mgmt: false,
            required_caps: None,
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
