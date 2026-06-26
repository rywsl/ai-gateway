//! `LlmGateway` — the `ProxyHttp` implementation that wires every Phase 1-3
//! crate together. Each method delegates to a `filters::*` module.
//!
//! Construction is via [`LlmGateway::new`] which takes pre-built `Arc`s for
//! every long-lived collaborator. The `gw-gateway` binary owns these and the
//! gateway itself is `Sync`-clean so Pingora can hand it to the service
//! framework.

use crate::ctx::{ArchiveState, ReqCtx};
use crate::filters;
use crate::resilience::ResilienceState;
use async_trait::async_trait;
use bytes::Bytes;
use gw_auth::ip_allowlist::{IpAllowlistGuard, IpAllowlistMode};
use gw_auth::ip_ratelimit::IpRateLimitGuard;
use gw_auth::jwt_blacklist::RedisJwtBlacklist;
use gw_auth::ratelimit::adaptive::AdaptiveRpmManager;
use gw_auth::ratelimit::RateLimitGuard;
use gw_codec::gemini::files::GeminiFilesGateway;
use gw_core::error::codes;
use gw_core::error::HeadersAccessor;
use gw_core::ids::{RequestId, TraceId};
use gw_core::ratelimit::{detect_rate_limit_kind, RateLimitKind};
use gw_health::KeyHealthStore;
use gw_ledger::ClearKey;
use gw_ledger::FreezeGuard;
use gw_ledger::Ledger;
use gw_ledger::LedgerItem;
use gw_routing::in_flight::InFlightStore;
use gw_routing::latency::LatencyTracker;
use gw_routing::{FailoverClass, FailoverDecision, FailoverExecutor};
use gw_routing::{ModelFrequencyTracker, ReservationGlobalView};
use gw_store::{CounterStore, EntityStore};
use pingora::http::{RequestHeader, ResponseHeader};
use pingora::upstreams::peer::HttpPeer;
use pingora::{Error, ErrorType, Result};
use pingora_proxy::{ProxyHttp, Session};
use rand::RngCore;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

/// Maximum cross-key connect retries per request (G10). At
/// `MAX_CONNECT_RETRIES = 2` a request can try at most three keys before
/// giving up: the initial pick plus two retries. Kept conservative because
/// the body is buffered in memory and each retry walks the strategy stack
/// again.
const MAX_CONNECT_RETRIES: u32 = 2;

/// Metric: count of grace-expired requests whose archive surfaces were
/// force-flushed (partial mode) in the `logging` filter. W6-T01 / W6-T07.
const ARCHIVE_EMIT_GRACE_FLUSH_TOTAL: &str = "gw_archive_emit_grace_flush_total";

/// Classification of a connect-time error for the G10 retry hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectErrorKind {
    /// TLS handshake / cert failure. Almost always a config issue (wrong
    /// SNI, bad CA, expired cert). Retrying a different key won't fix it,
    /// so we short-circuit immediately.
    Tls,
    /// Any other connect-time error (timeout, refused, no-route, proxy
    /// failure, generic handshake or connect). Retryable on another key.
    Connect,
}

/// Inspect the Pingora `Error` chain for known TLS error variants. The
/// `ErrorType` enum lives in `pingora-error`; the TLS group covers stages
/// from the X509 lookup through the post-handshake "other handshake" bucket.
///
/// Both the outer `etype` and the root (`root_etype`) are checked because
/// pingora sometimes wraps a low-level TLS failure inside a higher-level
/// `ConnectError`.
fn classify_connect_error(e: &Error) -> ConnectErrorKind {
    use ErrorType::*;
    let is_tls = |t: &ErrorType| {
        matches!(
            t,
            TLSWantX509Lookup
                | TLSHandshakeFailure
                | TLSHandshakeTimedout
                | InvalidCert
                | HandshakeError
        )
    };
    if is_tls(&e.etype) || is_tls(e.root_etype()) {
        ConnectErrorKind::Tls
    } else {
        ConnectErrorKind::Connect
    }
}

fn invalidate_connect_failure_affinity(
    ctx: &ReqCtx,
    affinity: &gw_routing::affinity::AffinityManager,
) {
    if let Some(key) = ctx.picked_vendor_key.as_ref() {
        affinity.invalidate_vendor_key(&key.key_id);
    }
}

fn remaining_failover_candidates(
    candidates: &[Arc<gw_core::entity::VendorKey>],
    ctx: &ReqCtx,
) -> Vec<Arc<gw_core::entity::VendorKey>> {
    let failed_key = ctx.picked_vendor_key.as_ref().map(|k| &k.key_id);
    candidates
        .iter()
        .filter(|candidate| {
            failed_key.is_none_or(|fk| candidate.key_id != *fk)
                && !ctx
                    .retry_blacklist
                    .iter()
                    .any(|tried| tried == &candidate.key_id)
        })
        .cloned()
        .collect()
}

fn failover_elapsed_ms(ctx: &ReqCtx) -> u64 {
    ctx.timing
        .start
        .map(|start| start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn next_failover_decision(
    executor: &FailoverExecutor,
    remaining: &[Arc<gw_core::entity::VendorKey>],
    attempt: u8,
    ctx: &ReqCtx,
) -> FailoverDecision {
    executor.next_candidate_with_elapsed(remaining, attempt, failover_elapsed_ms(ctx))
}

fn reset_response_attempt_state(ctx: &mut ReqCtx, new_key: Arc<gw_core::entity::VendorKey>) {
    ctx.inflight_guard = None;
    ctx.picked_vendor_key = Some(new_key);
    ctx.selected_endpoint_id = None;
    ctx.picked_vendor = None;
    ctx.upstream_codec = None;
    ctx.upstream_status = None;
    ctx.body_started = false;
    ctx.client_body_committed = false;
    ctx.stream_phase = crate::stream_probe::StreamPhase::Connecting;
    ctx.probe_started_at = None;
    ctx.probe_buffer.clear();
    ctx.pending_probe_failure = None;
    ctx.stream_probe_retry_pending = false;
    ctx.sse_leftover.clear();
    ctx.pending_client_body = None;
    ctx.sensitive_filter_transit = None;
    ctx.sensitive_filter_reverse_stream = None;
    // Reset archive accumulators. Without this, bytes from the failed attempt
    // bleed into the retry attempt's detailed-log surfaces, and any Spilled
    // shard files from the prior attempt would only be reclaimed by the LRU
    // enforcer. Assignment of the default variant drops the previous state,
    // which triggers `SpillFile::Drop` for Spilled variants.
    ctx.archive_upstream_response_body = ArchiveState::default();
    ctx.archive_downstream_response_body = ArchiveState::default();
}

fn adapter_body_head(body: &[u8]) -> &[u8] {
    let cap = body.len().min(8192);
    &body[..cap]
}

fn merge_adapter_error_outcome(
    mut base: gw_core::error::ClassifierOutcome,
    adapter: gw_adapter::ErrorOutcome,
) -> gw_core::error::ClassifierOutcome {
    if let Some(cooldown) = adapter.cooldown {
        base.cooldown = Some(cooldown);
    }
    if let Some(oauth_signal) = adapter.oauth_signal {
        base.oauth_signal = Some(oauth_signal);
    }
    if adapter.retry_after_secs.is_some() {
        base.rate_limit_hint = Some(gw_core::error::RateLimitHint {
            kind: gw_core::ratelimit::RateLimitKind::Unknown,
            retry_after_secs: adapter.retry_after_secs,
        });
    }
    match adapter.kind {
        Some(gw_adapter::ErrorKind::RateLimit) => {
            base.class = gw_core::error::ErrorClass::RateLimitedUpstream(
                gw_core::ratelimit::RateLimitKind::Unknown,
            );
            base.retryable = gw_core::error::Retryability::CrossKey;
        }
        Some(gw_adapter::ErrorKind::Quota) => {
            base.class = gw_core::error::ErrorClass::QuotaDenied;
            base.retryable = gw_core::error::Retryability::CrossKey;
        }
        Some(gw_adapter::ErrorKind::Authentication) => {
            base.class = gw_core::error::ErrorClass::AuthDenied;
        }
        Some(gw_adapter::ErrorKind::UpstreamServer | gw_adapter::ErrorKind::Transient) => {
            base.class = gw_core::error::ErrorClass::UpstreamTransient;
        }
        Some(gw_adapter::ErrorKind::Unknown) | None => {}
    }
    base
}

/// Task 4.4 — map an `X-Egress-Error-Class` header value to
/// `(stable_code, retryable, is_tls)`. `is_tls` short-circuits (no retry);
/// `retryable` routes through the FailoverExecutor; otherwise the error is
/// surfaced to the client as-is.
fn map_egress_class(class: &str) -> (&'static str, bool, bool) {
    use gw_core::error::codes::*;
    match class {
        "tls" => (GW_UPSTREAM_TLS_ERROR, false, true),
        "connect_timeout" => (GW_UPSTREAM_CONNECT_TIMEOUT, true, false),
        "connect_refused" => (GW_UPSTREAM_CONNECT_REFUSED, true, false),
        "read_timeout" => (GW_UPSTREAM_READ_TIMEOUT, true, false),
        "h2_protocol" => (GW_UPSTREAM_H2_PROTOCOL, true, false),
        "body_truncated" => (GW_UPSTREAM_BODY_TRUNCATED, false, false),
        "proxy_error" => (GW_UPSTREAM_PROXY_ERROR, true, false),
        "panic" => (GW_UPSTREAM_PANIC, false, false),
        _ => (GW_UPSTREAM_UNKNOWN, false, false),
    }
}

/// Map a pre-classified `ctx.upstream_error_code` to its observability
/// `ErrorKind` so `fail_to_proxy` can honour the precise code without
/// re-classifying via the generic HTTPStatus tail. The default arm picks
/// `UpstreamProtocol` to match the legacy HTTPStatus → PROTOCOL mapping.
fn kind_from_preset_code(code: &'static str) -> gw_core::observability::ErrorKind {
    use gw_core::error::codes::*;
    use gw_core::observability::ErrorKind;
    match code {
        // TLS / connect-layer failures: closer to a connect error than to a
        // wire-protocol violation.
        GW_UPSTREAM_TLS_ERROR
        | GW_UPSTREAM_CONNECT_FAILED
        | GW_UPSTREAM_CONNECT_TIMEOUT
        | GW_UPSTREAM_CONNECT_REFUSED
        | GW_UPSTREAM_READ_TIMEOUT
        | GW_UPSTREAM_PROXY_ERROR
        | GW_UPSTREAM_UNKNOWN
        | GW_UPSTREAM_PANIC => ErrorKind::UpstreamConnect,
        // Routing-layer exhaustion still surfaces as an upstream connect
        // problem from the client's POV (nowhere to send the request).
        GW_ROUTING_FAILOVER_TIMEOUT | GW_ROUTING_NO_FALLBACK_LEFT => ErrorKind::UpstreamConnect,
        GW_UPSTREAM_TRUNCATED | GW_UPSTREAM_BODY_TRUNCATED => ErrorKind::UpstreamMidStream,
        // Codec / protocol-decode failures.
        GW_UPSTREAM_H2_PROTOCOL | GW_UPSTREAM_PROTOCOL_DECODE_ERROR => ErrorKind::UpstreamProtocol,
        _ => ErrorKind::UpstreamProtocol,
    }
}

/// Gateway-internal loopback control + error headers. These are injected by
/// egress/tunnel loopbacks and must never be persisted into detailed archive.
/// Match is case-insensitive.
const LOOPBACK_INTERNAL_HEADERS: &[&str] = &[
    "x-egress-target",
    "x-egress-profile",
    "x-egress-proxy",
    "x-egress-error-class",
    "x-egress-error-detail",
    "x-gateway-tunnel-node-id",
    "x-gateway-tunnel-target",
    "x-gateway-tunnel-target-tls",
];

/// Drop any gateway-internal loopback headers from a captured header snapshot
/// before it is sent to the detailed archive. Preserves order of all other
/// headers.
fn redact_loopback_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .filter(|(name, _)| {
            let lower = name.to_ascii_lowercase();
            !LOOPBACK_INTERNAL_HEADERS.contains(&lower.as_str())
        })
        .collect()
}

fn mask_archive_header_values(
    policy: Option<&gw_runtime::CompiledSensitiveFilterPolicy>,
    headers: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let Some(policy) = policy else {
        return headers;
    };
    headers
        .into_iter()
        .map(|(name, value)| {
            let masked = policy.mask_archive_bytes(
                gw_core::entity::SensitiveFilterTarget::Archive,
                value.as_bytes(),
            );
            let value = String::from_utf8(masked.to_vec()).unwrap_or(value);
            (name, value)
        })
        .collect()
}

fn mask_archive_text(
    policy: Option<&gw_runtime::CompiledSensitiveFilterPolicy>,
    value: String,
) -> String {
    let Some(policy) = policy else {
        return value;
    };
    let masked = policy.mask_archive_bytes(
        gw_core::entity::SensitiveFilterTarget::Archive,
        value.as_bytes(),
    );
    String::from_utf8(masked.to_vec()).unwrap_or(value)
}

fn mask_archive_optional_text(
    policy: Option<&gw_runtime::CompiledSensitiveFilterPolicy>,
    value: Option<String>,
) -> Option<String> {
    value.map(|value| mask_archive_text(policy, value))
}

fn mask_archive_optional_id<T>(
    policy: Option<&gw_runtime::CompiledSensitiveFilterPolicy>,
    value: Option<T>,
) -> Option<T>
where
    T: AsRef<str> + From<String>,
{
    value.map(|value| mask_archive_text(policy, value.as_ref().to_owned()).into())
}

fn mask_archive_summary(
    policy: Option<&gw_runtime::CompiledSensitiveFilterPolicy>,
    summary: gw_ledger::detailed_archive::SummaryPayload,
) -> gw_ledger::detailed_archive::SummaryPayload {
    gw_ledger::detailed_archive::SummaryPayload {
        user_id: mask_archive_optional_id(policy, summary.user_id),
        api_key_id: mask_archive_optional_id(policy, summary.api_key_id),
        model: mask_archive_optional_id(policy, summary.model),
        client_protocol: mask_archive_optional_text(policy, summary.client_protocol),
        upstream_protocol: mask_archive_optional_text(policy, summary.upstream_protocol),
        http_status: summary.http_status,
        error_code: mask_archive_optional_text(policy, summary.error_code),
        is_stream: summary.is_stream,
        input_tokens: summary.input_tokens,
        output_tokens: summary.output_tokens,
        latency_ms: summary.latency_ms,
        ttft_ms: summary.ttft_ms,
        tunnel_resolution: mask_archive_optional_text(policy, summary.tunnel_resolution),
    }
}

fn mask_archive_body_with_encoding(
    policy: Option<&gw_runtime::CompiledSensitiveFilterPolicy>,
    body: Arc<Bytes>,
    encoding: ArchiveBodyEncoding,
) -> (Arc<Bytes>, u64) {
    let Some(policy) = policy else {
        return (body, 0);
    };
    match encoding {
        ArchiveBodyEncoding::Unsupported => {
            return (Arc::new(Bytes::new()), body.len() as u64);
        }
        ArchiveBodyEncoding::Supported(encoding) => {
            let Ok(decoded) = gw_codec::compression::decode(encoding, &body, 32 * 1024 * 1024)
            else {
                return (Arc::new(Bytes::new()), body.len() as u64);
            };
            let masked = policy
                .mask_archive_bytes(gw_core::entity::SensitiveFilterTarget::Archive, &decoded);
            if masked.as_ref() == decoded.as_ref() {
                return (body, 0);
            }
            let Ok(encoded) = gw_codec::compression::encode(encoding, &masked) else {
                return (Arc::new(Bytes::new()), body.len() as u64);
            };
            (Arc::new(encoded), 0)
        }
        ArchiveBodyEncoding::Identity => {
            let masked =
                policy.mask_archive_bytes(gw_core::entity::SensitiveFilterTarget::Archive, &body);
            if masked.as_ref() == body.as_ref() {
                (body, 0)
            } else {
                (Arc::new(masked), 0)
            }
        }
    }
}

fn mask_archive_surface(
    ctx: &ReqCtx,
    headers: Vec<(String, String)>,
    body: Arc<Bytes>,
) -> (Vec<(String, String)>, Arc<Bytes>, u64) {
    let policy = ctx.sensitive_filter_policy.as_deref();
    let encoding = archive_content_encoding(&headers);
    let (body, dropped) = mask_archive_body_with_encoding(policy, body, encoding);
    (mask_archive_header_values(policy, headers), body, dropped)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveBodyEncoding {
    Identity,
    Supported(gw_codec::compression::Encoding),
    Unsupported,
}

fn archive_content_encoding(headers: &[(String, String)]) -> ArchiveBodyEncoding {
    let mut values = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-encoding"))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty());
    let Some(value) = values.next() else {
        return ArchiveBodyEncoding::Identity;
    };
    if values.next().is_some() {
        return ArchiveBodyEncoding::Unsupported;
    }
    parse_archive_content_encoding(value)
}

fn parse_archive_content_encoding(value: &str) -> ArchiveBodyEncoding {
    let mut parts = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty());
    let Some(part) = parts.next() else {
        return ArchiveBodyEncoding::Identity;
    };
    if parts.next().is_some() {
        return ArchiveBodyEncoding::Unsupported;
    }
    if part.eq_ignore_ascii_case("identity") {
        ArchiveBodyEncoding::Identity
    } else if part.eq_ignore_ascii_case("gzip") {
        ArchiveBodyEncoding::Supported(gw_codec::compression::Encoding::Gzip)
    } else if part.eq_ignore_ascii_case("br") {
        ArchiveBodyEncoding::Supported(gw_codec::compression::Encoding::Br)
    } else {
        ArchiveBodyEncoding::Unsupported
    }
}

fn header_u32(response: &ResponseHeader, name: &str) -> Option<u32> {
    response
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u32>().ok())
}

/// First-present RPM-class rate-limit header (audit 2026-05-30 §3.9). Vendor
/// specific request-rate headers are preferred over the generic `x-ratelimit-limit`
/// so the adaptive learner ingests the most accurate signal. TPM/token headers
/// are intentionally excluded (Go-delegated per RL-Core).
fn rpm_limit_header(resp: &ResponseHeader) -> Option<u32> {
    [
        "anthropic-ratelimit-requests-limit",
        "x-ratelimit-remaining-requests",
        "x-ratelimit-limit",
        "x-rate-limit-limit",
        "ratelimit-limit",
        "rate-limit-limit",
    ]
    .iter()
    .find_map(|h| header_u32(resp, h))
}

/// True if the response carries a vendor-specific RPM-class header (NOT the
/// generic `x-ratelimit-limit`, which is too coarse to imply request-rate). Used
/// for the detection metric only.
fn detect_rpm_kind(resp: &ResponseHeader) -> bool {
    [
        "anthropic-ratelimit-requests-limit",
        "anthropic-ratelimit-requests-remaining",
        "x-ratelimit-remaining-requests",
    ]
    .iter()
    .any(|h| header_u32(resp, h).is_some())
}

struct ResponseHeaderAccessor<'a>(&'a ResponseHeader);

impl HeadersAccessor for ResponseHeaderAccessor<'_> {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.headers.get(name)?.to_str().ok()
    }
}

pub struct LlmGateway {
    pub store: Arc<EntityStore>,
    pub health: Arc<KeyHealthStore>,
    pub in_flight: Arc<InFlightStore>,
    pub latency_tracker: Arc<LatencyTracker>,
    pub ratelimit: Arc<RateLimitGuard>,
    pub ip_allowlist: Arc<IpAllowlistGuard>,
    pub ip_ratelimit: Arc<IpRateLimitGuard>,
    pub adaptive_rpm: Arc<AdaptiveRpmManager>,
    pub ip_allowlist_mode: IpAllowlistMode,
    pub ledger: Arc<Ledger>,
    pub counters: Arc<CounterStore>,
    pub global_rpm: u32,
    pub admin_upstream_url: Url,
    /// When `true`, parse the client IP from the `X-Forwarded-For` header
    /// only if the immediate peer is in `trusted_proxies`. When `false`, use
    /// the L4 socket peer address only. Default is `false` so a misconfigured
    /// deployment does not silently bypass IP blocklisting via a forged header.
    pub trust_xff: bool,
    pub trusted_proxies: Arc<Vec<ipnet::IpNet>>,
    pub signature_required: bool,
    pub signature_replay: Arc<gw_auth::signature::ReplayChecker>,
    pub task_enabled: bool,
    pub task_contract: Option<Arc<gw_task::redis_contract::RedisTaskContract>>,
    pub task_billing_config: Option<gw_billing::TaskBillingConfig>,
    /// JWT claim validator (iss/aud/nbf, T1-E), hot-reloadable (R7b). Swapped
    /// in place by the `events:entity ns=auth_config id=global` arm so
    /// `AuthConfig.global` rotation takes effect without a restart. A `None`
    /// inside the handle ⇒ auth falls back to signature-only checks
    /// (legacy `verify_jwt`), byte-identical to the pre-R7b `None` field.
    pub jwt_validator: Arc<arc_swap::ArcSwapOption<gw_auth::jwt::JwtValidator>>,
    /// Shared selection-strategy instances (T0-B). One `StrategyAny` per kind,
    /// dispatched via `StrategySet::pick_for`.
    pub strategies: Arc<gw_routing::strategy::set::StrategySet>,
    /// Request-external API-key affinity table shared with bootstrap
    /// invalidation hooks.
    pub affinity: Arc<gw_routing::affinity::AffinityManager>,
    /// Response-side failover executor (5xx / 429 before body starts).
    pub failover_executor: FailoverExecutor,
    /// Capability resolver (T3-C). `None` until wired; `filters::request`
    /// then leaves `ctx.required_caps` empty (v1 behaviour).
    pub capability_resolver: Option<Arc<gw_capability::resolver::CapabilityResolver>>,
    /// Management-token / JWT blacklist (T3-Pre wiring). Threaded into
    /// `filters::request::run` so T3-B can hook the mgmt verify chain.
    /// v1 holds the handle but does not consult it on the business path
    /// — that flips on once T3-B lands the verify call.
    pub mgmt_blacklist: Arc<RedisJwtBlacklist>,
    /// Resilience state (T3-A error-handler listeners; T3-Pre wiring).
    /// The proxy fanout updates this and the `/healthz/resilience` admin
    /// surface (T3-C) renders it via `ResilienceState::snapshot_json`.
    pub resilience: Arc<ResilienceState>,
    /// Tunnel decision stack (ADR-T-2). `None` ⇒ pick_peer always picks Direct,
    /// behaviour byte-identical to pre-tunnel builds. Built by
    /// `gw-gateway::bootstrap` only when `cfg.tunnel.enabled = true`.
    pub tunnel: Option<Arc<crate::tunnel_stack::TunnelStack>>,
    /// Egress impersonation handle (Phase 4). Holds the loopback listener bind
    /// (`EgressBind::Uds | Tcp`) that `pick_peer` points an `HttpPeer` at when
    /// the selected vendor's `egress_mode = Impersonate`. Same architectural
    /// neighborhood as `tunnel` since both gate the upstream peer construction.
    pub egress: Arc<gw_egress_impersonate::EgressHandle>,
    /// Kill-switch for the egress-impersonate path (Task 5.2). When `true`,
    /// vendors with `egress_mode = Impersonate` are excluded from candidate
    /// selection in `pick_peer`; metric `gw_egress_disabled_total` ticks per
    /// dropped vendor. Rustls vendors are unaffected. Boot-time only.
    pub egress_disabled: bool,
    /// Identity-persist queue (Task 4.6). Lazy snapshot writer for forged
    /// identities; cloned onto `ReqCtx` in `new_ctx`.
    pub identity_persist: Arc<gw_store::identity_persist::IdentityPersistQueue>,
    /// Turn-state cache (Task 4.6). Per-conversation impersonation turn
    /// counter; cloned onto `ReqCtx` in `new_ctx`.
    pub turn_state_cache: Arc<gw_core::impersonate::turn_state::TurnStateCache>,
    /// Detailed-log archive (T10). Sender handle for per-surface archive
    /// capture. `try_send` is fire-and-forget; a full channel drops + metrics.
    pub(crate) detailed_archive: std::sync::Arc<gw_ledger::detailed_archive::DetailedArchive>,
    /// InflightLedger (W2-T6). Tracks in-flight request hashes in Redis so the
    /// W4 crash-recovery reconciler can detect billing gaps after a restart.
    pub(crate) inflight: std::sync::Arc<gw_ledger::inflight::InflightLedger>,
    /// Response deadline (W3-T3). Maximum time from `Sent` to EOS before the
    /// upstream stream is aborted and `CancelCause::Deadline` is recorded.
    /// Populated from `cfg.billing.response_deadline_ms` at boot time.
    pub(crate) response_deadline_ms: u64,
    /// Cancel grace window (W3-T5). After a cancel trigger fires, the gateway
    /// continues reading the upstream body for this many milliseconds to
    /// capture a final usage frame the vendor may still be sending.
    /// Populated from `cfg.billing.cancel_grace_ms` at boot time.
    pub(crate) cancel_grace_ms: u64,
    /// WS-15/T15.7 rollout flag for non-stream response parse failures.
    pub(crate) r7_502_on_parse_fail: bool,
    /// WS-15/T15.4 license enforcement gate (hot-reloadable). Consulted in
    /// `request_filter` so a licence violation is gated by
    /// `[flags].license_enforce`. Real `LicenseStatus` arrives with WS-5 /
    /// WS-16; until then the skeleton calls `enforce` with `Valid` so the
    /// 402 rejection path sits inert.
    pub(crate) license_gate: Arc<gw_auth::license::LicenseGate>,
    /// Hard-cap configuration (ADR-024). Threaded from `cfg.auth.*` at boot.
    pub(crate) hard_cap_cfg: gw_auth::hard_cap::HardCapCfg,
    /// Credit-lease protection-mode config (Spec 2026-05-29-credit-lease-design).
    /// Read by `request_filter` step 8.5 to decide whether the request goes
    /// through the lease branch or the legacy soft-defense path.
    pub(crate) protection_cfg: gw_ledger::ProtectionConfig,
    pub(crate) overflow_gate: std::sync::Arc<gw_ledger::overflow::OverflowGate>,
    pub(crate) active_registration: gw_ledger::ActiveRegistrationHandle,
    pub(crate) classifier: Arc<dyn gw_core::error::ErrorClassifier>,
    pub(crate) adapter_registry: Arc<gw_adapter::AdapterRegistry>,
    pub(crate) oauth_runtime: Arc<dyn gw_adapter::OAuthRuntime>,
    pub(crate) quota_state_updates: Option<Arc<gw_store::quota_state::QuotaStateUpdateQueue>>,
    pub(crate) retryable_codes: Arc<std::collections::HashSet<u16>>,
    pub(crate) error_passthrough_enabled: bool,
    /// Ordered allow/deny rules over the upstream error message (audit §3.8).
    /// Empty = pass every message through when enabled (prior behavior).
    pub(crate) error_passthrough_rules: Arc<Vec<gw_core::error::rules::PassthroughRule>>,
    pub(crate) error_message_rules: Arc<gw_store::error_message_rules::ErrorMessageRuleStore>,
    pub files_gateway: Arc<dyn GeminiFilesGateway>,
    pub reservation_view: Option<Arc<ReservationGlobalView>>,
    pub frequency_tracker: Option<Arc<ModelFrequencyTracker>>,
    pub body_rules: Arc<
        std::collections::HashMap<
            String,
            std::collections::HashMap<String, Box<serde_json::value::RawValue>>,
        >,
    >,
    /// Sharded spill root for archive disk-overflow (T5.2). Cloned into each
    /// `ReqCtx` in `new_ctx`.
    pub spill_root: Option<Arc<gw_runtime::SpillRoot>>,
    /// Archive cap policy (soft/hard cap) for response-body accumulation
    /// (T5.2). Cloned into each `ReqCtx` in `new_ctx`.
    pub archive_caps: crate::ctx::ArchiveCaps,
    /// Compiled-regex cache (T6.1). Wired from Wired at bootstrap; cloned
    /// into each `ReqCtx` in `new_ctx` so ModelPermission::new can call
    /// `cache.get_or_compile` on the hot path without per-request compilation.
    pub regex_cache: Arc<gw_runtime::RegexCache>,
    /// Force-majeure backpressure gate (T7.1). Wired from Wired at bootstrap;
    /// cloned into each `ReqCtx` in `new_ctx`.
    pub backpressure_gate: Arc<gw_runtime::BackpressureGate>,
}

impl LlmGateway {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<EntityStore>,
        health: Arc<KeyHealthStore>,
        in_flight: Arc<InFlightStore>,
        latency_tracker: Arc<LatencyTracker>,
        ratelimit: Arc<RateLimitGuard>,
        ip_allowlist: Arc<IpAllowlistGuard>,
        ip_ratelimit: Arc<IpRateLimitGuard>,
        adaptive_rpm: Arc<AdaptiveRpmManager>,
        ip_allowlist_mode: IpAllowlistMode,
        ledger: Arc<Ledger>,
        counters: Arc<CounterStore>,
        global_rpm: u32,
        admin_upstream_url: Url,
        trust_xff: bool,
        trusted_proxies: Arc<Vec<ipnet::IpNet>>,
        signature_required: bool,
        signature_replay: Arc<gw_auth::signature::ReplayChecker>,
        task_enabled: bool,
        task_contract: Option<Arc<gw_task::redis_contract::RedisTaskContract>>,
        task_billing_config: Option<gw_billing::TaskBillingConfig>,
        jwt_validator: Arc<arc_swap::ArcSwapOption<gw_auth::jwt::JwtValidator>>,
        strategies: Arc<gw_routing::strategy::set::StrategySet>,
        affinity: Arc<gw_routing::affinity::AffinityManager>,
        failover_executor: FailoverExecutor,
        capability_resolver: Option<Arc<gw_capability::resolver::CapabilityResolver>>,
        mgmt_blacklist: Arc<RedisJwtBlacklist>,
        resilience: Arc<ResilienceState>,
        tunnel: Option<Arc<crate::tunnel_stack::TunnelStack>>,
        egress: Arc<gw_egress_impersonate::EgressHandle>,
        egress_disabled: bool,
        identity_persist: Arc<gw_store::identity_persist::IdentityPersistQueue>,
        turn_state_cache: Arc<gw_core::impersonate::turn_state::TurnStateCache>,
        detailed_archive: std::sync::Arc<gw_ledger::detailed_archive::DetailedArchive>,
        inflight: std::sync::Arc<gw_ledger::inflight::InflightLedger>,
        response_deadline_ms: u64,
        cancel_grace_ms: u64,
        r7_502_on_parse_fail: bool,
        license_gate: Arc<gw_auth::license::LicenseGate>,
        hard_cap_cfg: gw_auth::hard_cap::HardCapCfg,
        protection_cfg: gw_ledger::ProtectionConfig,
        overflow_gate: std::sync::Arc<gw_ledger::overflow::OverflowGate>,
        active_registration: gw_ledger::ActiveRegistrationHandle,
        classifier: Arc<dyn gw_core::error::ErrorClassifier>,
        adapter_registry: Arc<gw_adapter::AdapterRegistry>,
        oauth_runtime: Arc<dyn gw_adapter::OAuthRuntime>,
        quota_state_updates: Option<Arc<gw_store::quota_state::QuotaStateUpdateQueue>>,
        retryable_codes: Arc<std::collections::HashSet<u16>>,
        error_passthrough_enabled: bool,
        error_passthrough_rules: Arc<Vec<gw_core::error::rules::PassthroughRule>>,
        error_message_rules: Arc<gw_store::error_message_rules::ErrorMessageRuleStore>,
        reservation_view: Option<Arc<ReservationGlobalView>>,
        frequency_tracker: Option<Arc<ModelFrequencyTracker>>,
        pg: sqlx::PgPool,
        regex_cache: Arc<gw_runtime::RegexCache>,
        backpressure_gate: Arc<gw_runtime::BackpressureGate>,
    ) -> Self {
        let files_gateway = Arc::new(crate::filters::gemini_files::HttpGeminiFilesGateway::new(
            pg,
            store.clone(),
            health.clone(),
            in_flight.clone(),
            strategies.clone(),
            latency_tracker.clone(),
        ));
        Self {
            store,
            health,
            in_flight,
            latency_tracker,
            ratelimit,
            ip_allowlist,
            ip_ratelimit,
            adaptive_rpm,
            ip_allowlist_mode,
            ledger,
            counters,
            global_rpm,
            admin_upstream_url,
            trust_xff,
            trusted_proxies,
            signature_required,
            signature_replay,
            task_enabled,
            task_contract,
            task_billing_config,
            jwt_validator,
            strategies,
            affinity,
            failover_executor,
            capability_resolver,
            mgmt_blacklist,
            resilience,
            tunnel,
            egress,
            egress_disabled,
            identity_persist,
            turn_state_cache,
            detailed_archive,
            inflight,
            response_deadline_ms,
            cancel_grace_ms,
            r7_502_on_parse_fail,
            license_gate,
            hard_cap_cfg,
            protection_cfg,
            overflow_gate,
            active_registration,
            classifier,
            adapter_registry,
            oauth_runtime,
            quota_state_updates,
            retryable_codes,
            error_passthrough_enabled,
            error_passthrough_rules,
            error_message_rules,
            files_gateway,
            reservation_view,
            frequency_tracker,
            body_rules: Arc::new(std::collections::HashMap::new()),
            spill_root: None,
            archive_caps: crate::ctx::ArchiveCaps::default(),
            regex_cache,
            backpressure_gate,
        }
    }

    /// Accessor for the shared `ResilienceState`. Used by `gw-gateway` /
    /// integration tests that need to observe the same counters the proxy
    /// fanout writes. The field is already `pub` so direct access works,
    /// but this returns the borrow without cloning the `Arc`.
    pub fn resilience(&self) -> &Arc<ResilienceState> {
        &self.resilience
    }

    async fn classify_final_upstream_error(&self, ctx: &mut ReqCtx) {
        let Some(status) = ctx.upstream_status else {
            return;
        };
        if status < 400 {
            return;
        }
        let Some(meta) = ctx.archive_upstream_response_meta.as_ref() else {
            return;
        };
        let picked = ctx.picked_vendor_key.as_ref();
        let classify_ctx = gw_core::error::ClassifyCtx {
            key_id: picked.map_or("", |k| k.key_id.0.as_str()),
            vendor_id: picked.map_or("", |k| k.vendor_id.0.as_str()),
            api_format: ctx
                .client_codec
                .as_ref()
                .map_or(gw_core::protocol::ProtocolKind::OpenAi, |c| c.kind()),
            protocol: ctx
                .upstream_codec
                .as_ref()
                .map_or(gw_core::protocol::ProtocolKind::OpenAi, |c| c.kind()),
            has_oauth: picked.is_some_and(|k| k.oauth.is_some()),
        };
        struct MetaHeaders<'a>(&'a [(String, String)]);
        impl gw_core::error::HeadersAccessor for MetaHeaders<'_> {
            fn get(&self, name: &str) -> Option<&str> {
                self.0
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v.as_str())
            }
        }
        impl gw_adapter::HeaderView for MetaHeaders<'_> {
            fn get(&self, name: &str) -> Option<&str> {
                <Self as gw_core::error::HeadersAccessor>::get(self, name)
            }
        }

        // Read archive bytes for the adapter analyzer. On read failure (e.g.
        // SpillFile I/O error) fall back to an empty slice so classification
        // still proceeds on the status + headers alone.
        let upstream_bytes = match ctx.archive_upstream_response_body.read_for_emit().await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    target: "gw_proxy::archive",
                    request_id = %ctx.request_id,
                    surface = "upstream",
                    error = %e,
                    "archive_emit_read_failed"
                );
                Vec::new()
            }
        };

        let headers = MetaHeaders(&meta.headers);
        let base = self
            .classifier
            .classify_upstream(&classify_ctx, status, &headers, None);
        let adapter_outcome = picked
            .map(|key| {
                self.adapter_registry
                    .error_analyzer_for(&key.vendor_id)
                    .analyze(status, adapter_body_head(&upstream_bytes), &headers)
            })
            .unwrap_or_default();
        ctx.error_outcome = Some(merge_adapter_error_outcome(base, adapter_outcome));
    }

    /// Emit upstream + downstream response surfaces at end-of-stream.
    /// Called from the `logging` filter (W6-T07) once per request.
    ///
    /// When `force_flush_partial` is true (W6-T01: grace-expired path), a
    /// missing `archive_upstream_response_meta` is synthesized from
    /// `ctx.upstream_status` so partial SSE/JSON already received is still
    /// archived rather than silently dropped. Internal failures remain
    /// best-effort (try_send + warn); the outer request flow is unaffected.
    async fn emit_response_surfaces(&self, ctx: &mut ReqCtx, force_flush_partial: bool) {
        use crate::ctx::{ArchiveState, ResponseMeta};
        use gw_ledger::detailed_archive::{CapturedSurface, Kind, ResponsePayload, SurfacePayload};
        let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;

        // upstream_response — paired with the headers snapshot taken in response_filter.
        // W6-T01: when force_flush_partial is true and no meta was captured
        // (e.g. cancel-grace-expired before response_filter committed headers),
        // synthesize a minimal meta so the partial body is still archived.
        let upstream_meta = ctx.archive_upstream_response_meta.take().or_else(|| {
            if force_flush_partial {
                Some(ResponseMeta {
                    status: ctx.upstream_status.unwrap_or(0),
                    headers: Vec::new(),
                })
            } else {
                None
            }
        });
        if let Some(meta) = upstream_meta {
            let body_bytes = match ctx.archive_upstream_response_body.read_for_emit().await {
                Ok(b) => Bytes::from(b),
                Err(e) => {
                    tracing::warn!(
                        target: "gw_proxy::archive",
                        request_id = %ctx.request_id,
                        surface = "upstream",
                        error = %e,
                        "archive_emit_read_failed"
                    );
                    Bytes::new()
                }
            };
            let truncated = ctx.archive_upstream_response_body.is_truncated();
            let dropped = ctx.archive_upstream_response_body.dropped();
            let body = std::sync::Arc::new(body_bytes);
            let (headers, body, mask_dropped) =
                mask_archive_surface(ctx, redact_loopback_headers(meta.headers), body);
            let dropped = dropped.saturating_add(mask_dropped);
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: now_ms,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                attempt_index: ctx.attempt_index,
                kind: Kind::UpstreamResponse,
                payload: SurfacePayload::Response(
                    ResponsePayload {
                        status: meta.status,
                        headers,
                        body_b64: String::new(),
                        archive_truncated: truncated,
                        archive_dropped: dropped,
                    },
                    body,
                ),
            });
            // Release spill file / reset accumulator.
            ctx.archive_upstream_response_body = ArchiveState::default();
        }

        // downstream_response — v1 approximates headers from upstream snapshot.
        let downstream_meta = ctx.archive_downstream_response_meta.take().or_else(|| {
            if !force_flush_partial || ctx.upstream_status.is_some() {
                Some(ResponseMeta {
                    status: ctx.upstream_status.unwrap_or(0),
                    headers: Vec::new(),
                })
            } else {
                None
            }
        });
        if let Some(downstream_meta) = downstream_meta {
            let body_bytes = match ctx.archive_downstream_response_body.read_for_emit().await {
                Ok(b) => Bytes::from(b),
                Err(e) => {
                    tracing::warn!(
                        target: "gw_proxy::archive",
                        request_id = %ctx.request_id,
                        surface = "downstream",
                        error = %e,
                        "archive_emit_read_failed"
                    );
                    Bytes::new()
                }
            };
            let truncated = ctx.archive_downstream_response_body.is_truncated();
            let dropped = ctx.archive_downstream_response_body.dropped();
            let body = std::sync::Arc::new(body_bytes);
            let (headers, body, mask_dropped) =
                mask_archive_surface(ctx, redact_loopback_headers(downstream_meta.headers), body);
            let dropped = dropped.saturating_add(mask_dropped);
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: now_ms,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                attempt_index: ctx.attempt_index,
                kind: Kind::DownstreamResponse,
                payload: SurfacePayload::Response(
                    ResponsePayload {
                        status: downstream_meta.status,
                        headers,
                        body_b64: String::new(),
                        archive_truncated: truncated,
                        archive_dropped: dropped,
                    },
                    body,
                ),
            });
            // Release spill file / reset accumulator.
            ctx.archive_downstream_response_body = ArchiveState::default();
        }
    }

    fn read_archive_for_emit_blocking(
        state: &ArchiveState,
        request_id: &RequestId,
        surface: &'static str,
    ) -> Bytes {
        let bytes = match state {
            ArchiveState::InMemory(_) | ArchiveState::Truncated { .. } => {
                state.read_for_emit_sync()
            }
            ArchiveState::Spilled { .. } => tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(async {
                    match state.read_for_emit().await {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::warn!(
                                target: "gw_proxy::archive",
                                request_id = %request_id,
                                surface,
                                error = %e,
                                "archive_emit_read_failed"
                            );
                            Vec::new()
                        }
                    }
                })
            }),
        };
        Bytes::from(bytes)
    }

    fn emit_response_surfaces_sync(&self, ctx: &mut ReqCtx, force_flush_partial: bool) {
        use crate::ctx::ResponseMeta;
        use gw_ledger::detailed_archive::{CapturedSurface, Kind, ResponsePayload, SurfacePayload};
        let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;

        let upstream_meta = ctx.archive_upstream_response_meta.take().or_else(|| {
            if force_flush_partial {
                Some(ResponseMeta {
                    status: ctx.upstream_status.unwrap_or(0),
                    headers: Vec::new(),
                })
            } else {
                None
            }
        });
        if let Some(meta) = upstream_meta {
            let body = std::sync::Arc::new(Self::read_archive_for_emit_blocking(
                &ctx.archive_upstream_response_body,
                &ctx.request_id,
                "upstream",
            ));
            let truncated = ctx.archive_upstream_response_body.is_truncated();
            let dropped = ctx.archive_upstream_response_body.dropped();
            let (headers, body, mask_dropped) =
                mask_archive_surface(ctx, redact_loopback_headers(meta.headers), body);
            let dropped = dropped.saturating_add(mask_dropped);
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: now_ms,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                attempt_index: ctx.attempt_index,
                kind: Kind::UpstreamResponse,
                payload: SurfacePayload::Response(
                    ResponsePayload {
                        status: meta.status,
                        headers,
                        body_b64: String::new(),
                        archive_truncated: truncated,
                        archive_dropped: dropped,
                    },
                    body,
                ),
            });
            ctx.archive_upstream_response_body = ArchiveState::default();
        }

        let downstream_meta = ctx.archive_downstream_response_meta.take().or_else(|| {
            if !force_flush_partial || ctx.upstream_status.is_some() {
                Some(ResponseMeta {
                    status: ctx.upstream_status.unwrap_or(0),
                    headers: Vec::new(),
                })
            } else {
                None
            }
        });
        if let Some(meta) = downstream_meta {
            let body = std::sync::Arc::new(Self::read_archive_for_emit_blocking(
                &ctx.archive_downstream_response_body,
                &ctx.request_id,
                "downstream",
            ));
            let truncated = ctx.archive_downstream_response_body.is_truncated();
            let dropped = ctx.archive_downstream_response_body.dropped();
            let (headers, body, mask_dropped) =
                mask_archive_surface(ctx, redact_loopback_headers(meta.headers), body);
            let dropped = dropped.saturating_add(mask_dropped);
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: now_ms,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                attempt_index: ctx.attempt_index,
                kind: Kind::DownstreamResponse,
                payload: SurfacePayload::Response(
                    ResponsePayload {
                        status: meta.status,
                        headers,
                        body_b64: String::new(),
                        archive_truncated: truncated,
                        archive_dropped: dropped,
                    },
                    body,
                ),
            });
            ctx.archive_downstream_response_body = ArchiveState::default();
        }
    }

    fn retry_stream_probe_failure(
        &self,
        ctx: &mut ReqCtx,
        failure: crate::stream_probe::ProbeFailure,
    ) -> Result<bool> {
        if !crate::stream_probe::can_retry_probe_failure(ctx, failure) {
            return Ok(false);
        }

        ctx.pending_probe_failure = Some(failure);
        ctx.stream_probe_retry_pending = true;
        let strategy_name = ctx.strategy_name.unwrap_or("unknown");
        let Some(candidates) = ctx.picked_candidates.as_ref() else {
            ctx.upstream_error_code = Some(codes::GW_ROUTING_NO_FALLBACK_LEFT);
            metrics::counter!(
                "gw_stream_probe_failover_total",
                "reason" => failure.as_label(),
                "strategy" => strategy_name.to_owned(),
                "outcome" => "exhausted",
            )
            .increment(1);
            return Ok(false);
        };
        let remaining = remaining_failover_candidates(candidates, ctx);
        let attempt = ctx.failover_attempt.saturating_add(1);
        let decision = next_failover_decision(&self.failover_executor, &remaining, attempt, ctx);

        match decision {
            FailoverDecision::Retry(new_key) => {
                self.emit_response_surfaces_sync(ctx, true);
                crate::billing_lifecycle::settle_prior_attempt_spawn(
                    ctx,
                    self.inflight.clone(),
                    self.ledger.clone(),
                );
                if let Some(k) = ctx.picked_vendor_key.as_ref() {
                    if !ctx.retry_blacklist.iter().any(|id| id == &k.key_id) {
                        ctx.retry_blacklist.push(k.key_id.clone());
                    }
                }
                ctx.failover_attempt = attempt;
                reset_response_attempt_state(ctx, new_key);
                ctx.stream_probe_retry_pending = true;
                ctx.pending_probe_failure = Some(failure);
                metrics::counter!(
                    "gw_stream_probe_failover_total",
                    "reason" => failure.as_label(),
                    "strategy" => strategy_name.to_owned(),
                    "outcome" => "retry",
                )
                .increment(1);
                Ok(true)
            }
            FailoverDecision::Exhausted => {
                ctx.upstream_error_code = Some(codes::GW_ROUTING_NO_FALLBACK_LEFT);
                metrics::counter!(
                    "gw_stream_probe_failover_total",
                    "reason" => failure.as_label(),
                    "strategy" => strategy_name.to_owned(),
                    "outcome" => "exhausted",
                )
                .increment(1);
                Ok(false)
            }
            FailoverDecision::Timeout => {
                ctx.upstream_error_code = Some(codes::GW_ROUTING_FAILOVER_TIMEOUT);
                metrics::counter!(
                    "gw_stream_probe_failover_total",
                    "reason" => "timeout",
                    "strategy" => strategy_name.to_owned(),
                    "outcome" => "timeout",
                )
                .increment(1);
                Ok(false)
            }
            FailoverDecision::NotRetryable => Ok(false),
        }
    }
}

#[async_trait]
impl ProxyHttp for LlmGateway {
    type CTX = ReqCtx;

    fn new_ctx(&self) -> Self::CTX {
        let id = trace_id_hex();
        let trace = TraceId::from(id.clone());
        let req = RequestId::from(id);
        let mut ctx = ReqCtx::new(
            trace,
            req,
            self.regex_cache.clone(),
            self.backpressure_gate.clone(),
        );
        ctx.reservation_view = self.reservation_view.clone();
        ctx.frequency_tracker = self.frequency_tracker.clone();
        ctx.identity_persist = Some(self.identity_persist.clone());
        ctx.turn_state_cache = Some(self.turn_state_cache.clone());
        ctx.spill_root = self.spill_root.clone();
        ctx.archive_caps = self.archive_caps;
        ctx.r7_502_on_parse_fail = self.r7_502_on_parse_fail;
        ctx
    }

    async fn early_request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()> {
        filters::early::run(
            session,
            ctx,
            filters::early::EarlyFilterDeps {
                store: &self.store,
                ratelimit: &self.ratelimit,
                ip_allowlist: &self.ip_allowlist,
                ip_ratelimit: &self.ip_ratelimit,
                ip_allowlist_mode: self.ip_allowlist_mode,
                trust_xff: self.trust_xff,
                trusted_proxies: &self.trusted_proxies,
                task_enabled: self.task_enabled,
            },
        )
        .await
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        // WS-15/T15.4: license gate. Real `LicenseStatus` arrives with WS-5 /
        // WS-16; until then the gate is wired but inert (Valid status never
        // violates). The `enforce` Err arm carries the 402 `GW_LICENSE_REQUIRED`
        // decision — mapping it to an HTTP 402 response lands alongside the
        // real status source (the rejection path is exercised by `license::tests`).
        let license_status = gw_auth::license::LicenseStatus::Valid;
        if let Err(lic_err) = self
            .license_gate
            .enforce(&gw_auth::license::LicenseContext {
                license_status: &license_status,
                feature: None,
            })
        {
            tracing::warn!(
                target: "gw_gateway",
                error = ?lic_err,
                "license gate rejected request (skeleton: status source pending WS-5/WS-16)"
            );
            // TODO(WS-5/WS-16): once a real LicenseStatus source feeds a
            // violation, emit ErrorIR{status:402, code:GW_LICENSE_REQUIRED}
            // and short-circuit (Ok(true)). Skeleton status is always Valid,
            // so this branch is not reached yet.
        }
        // R7b: snapshot the current JWT validator per-request. `load_full` is
        // lock-free and yields an owned `Option<Arc<JwtValidator>>` safe to hold
        // across the `.await` below; a concurrent hot-reload swaps the handle so
        // the next request observes the new validator without a restart.
        let jwt_validator = self.jwt_validator.load_full();
        let short_circuit = filters::request::run(
            session,
            ctx,
            &self.store,
            &self.ratelimit,
            &self.ip_allowlist,
            &self.ip_ratelimit,
            self.ip_allowlist_mode,
            &self.counters,
            self.global_rpm,
            self.trust_xff,
            &self.trusted_proxies,
            self.signature_required,
            &self.signature_replay,
            self.task_enabled,
            self.task_contract.as_deref(),
            self.task_billing_config,
            jwt_validator.as_ref(),
            self.capability_resolver.as_ref(),
            &self.mgmt_blacklist,
            &self.hard_cap_cfg,
            &self.protection_cfg,
            &self.overflow_gate,
            &self.active_registration,
            self.files_gateway.as_ref(),
            Some(filters::request::GeminiFilesRoutingDeps {
                store: &self.store,
                health: &self.health,
                in_flight: &self.in_flight,
                strategies: &self.strategies,
                latency_tracker: &self.latency_tracker,
            }),
        )
        .await?;
        ctx.sensitive_filter_policy = self
            .store
            .compiled_sensitive_filter_policy_or_compile()
            .await;
        // Emit downstream_request surface (only when process() actually read the
        // body; short-circuit paths may bail before the snapshot is taken).
        if let (Some(body), Some(meta)) = (
            ctx.downstream_request_body.clone(),
            ctx.downstream_request_meta.clone(),
        ) {
            use gw_ledger::detailed_archive::{
                CapturedSurface, Kind, RequestPayload, SurfacePayload,
            };
            let policy = ctx.sensitive_filter_policy.as_deref();
            let method = mask_archive_text(policy, meta.method);
            let url = mask_archive_text(policy, meta.url);
            let http_version = mask_archive_text(policy, meta.http_version);
            let (headers, body, _mask_dropped) =
                mask_archive_surface(ctx, redact_loopback_headers(meta.headers), body);
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                attempt_index: ctx.attempt_index,
                kind: Kind::DownstreamRequest,
                payload: SurfacePayload::Request(
                    RequestPayload {
                        method,
                        url,
                        http_version,
                        headers,
                        body_b64: String::new(),
                    },
                    body,
                ),
            });
        }
        Ok(short_circuit)
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        ctx.phase_start("upstream_peer");
        if ctx.mgmt {
            let url = &self.admin_upstream_url;
            let host = url
                .host_str()
                .ok_or_else(|| Error::new(ErrorType::Custom("gw_admin_upstream_no_host")))?;
            let port = url
                .port_or_known_default()
                .ok_or_else(|| Error::new(ErrorType::Custom("gw_admin_upstream_no_port")))?;
            let tls = match url.scheme() {
                "http" => false,
                "https" => true,
                _ => {
                    return Err(Error::new(ErrorType::Custom(
                        "gw_admin_upstream_bad_scheme",
                    )))
                }
            };
            let peer = HttpPeer::new(format!("{host}:{port}"), tls, host.to_string());
            ctx.phase_end();
            return Ok(Box::new(peer));
        }
        let peer = filters::upstream::pick_peer(
            session,
            ctx,
            &self.store,
            &self.health,
            &self.in_flight,
            &self.strategies,
            &self.affinity,
            &self.latency_tracker,
            &self.adaptive_rpm,
            &self.ratelimit,
            &self.egress,
            self.egress_disabled,
            self.tunnel.as_ref(),
        )
        .await?;
        ctx.phase_end();
        Ok(peer)
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        filters::upstream::rewrite_request(
            session,
            upstream_request,
            ctx,
            &self.body_rules,
            &self.adapter_registry,
            &self.oauth_runtime,
            &self.health,
        )
        .await?;
        filters::upstream::record_api_key_affinity(ctx, self.affinity.as_ref());

        // W2-T6: Register the in-flight billing entry now that the request is
        // committed to being sent to the upstream. Best-effort — Redis errors
        // are warned but never propagate to the client.
        {
            use gw_ledger::inflight::InflightRegistration;
            let registration = InflightRegistration {
                request_id: ctx.request_id.0.clone(),
                attempt_index: ctx.attempt_index,
                user_id: ctx
                    .effective_user_id()
                    .map(|u| u.to_string())
                    .unwrap_or_default(),
                vendor_key_id: ctx
                    .picked_vendor_key
                    .as_ref()
                    .map(|vk| vk.key_id.to_string())
                    .unwrap_or_default(),
                freeze_amount: ctx
                    .freeze_guard
                    .as_ref()
                    .map(|g| g.freeze_amount())
                    .unwrap_or(0),
                prompt_estimate_tokens: ctx.prompt_estimate_tokens.unwrap_or(0),
            };
            if let Err(e) = self.inflight.register(&registration).await {
                tracing::warn!(
                    error = ?e,
                    request_id = %ctx.request_id,
                    "inflight register failed; proceeding without crash-safety net"
                );
                metrics::counter!("gw_billing_inflight_register_failure_total").increment(1);
            }
        }

        if let (Some(body), Some(meta)) = (
            ctx.upstream_request_body.clone(),
            ctx.upstream_request_meta.clone(),
        ) {
            use gw_ledger::detailed_archive::{
                CapturedSurface, Kind, RequestPayload, SurfacePayload,
            };
            let policy = ctx.sensitive_filter_policy.as_deref();
            let method = mask_archive_text(policy, meta.method);
            let url = mask_archive_text(policy, meta.url);
            let http_version = mask_archive_text(policy, meta.http_version);
            let (headers, body, _mask_dropped) =
                mask_archive_surface(ctx, redact_loopback_headers(meta.headers), body);
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                attempt_index: ctx.attempt_index,
                kind: Kind::UpstreamRequest,
                payload: SurfacePayload::Request(
                    RequestPayload {
                        method,
                        url,
                        http_version,
                        headers,
                        body_b64: String::new(),
                    },
                    body,
                ),
            });
        }
        if ctx.egress_profile.is_some() {
            ctx.egress_sent_at = Some(std::time::Instant::now());
        }
        Ok(())
    }

    /// Replace the upstream-bound body with the codec-rewritten payload (if any).
    ///
    /// `upstream_request_filter` stored the final outbound bytes in the body
    /// view via `set_outbound`. Without this hook, pingora would forward the
    /// ORIGINAL downstream body to the upstream, so cross-protocol re-renders
    /// would never reach the wire.
    ///
    /// For same-protocol passthrough the rewritten body equals the original, so
    /// the conditional replacement is still correct. We only swap on the final
    /// chunk so chunked-encoding clients still stream cleanly through pingora.
    ///
    /// `Bytes` is a cheap Arc clone; `outbound_or_raw()` returns a reference to
    /// the stored bytes so the clone is effectively free.
    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        if end_of_stream {
            if let Some(view) = ctx.body.as_ref() {
                *body = Some(view.outbound_or_raw().clone());
            }
        } else {
            // Drop pre-EOF chunks: the full body was already buffered in
            // `request_filter` and will be emitted in one shot at EOS.
            *body = None;
        }
        Ok(())
    }

    async fn response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        ctx.upstream_status = Some(upstream_response.status.as_u16());
        // Egress-impersonate loopback latency (spec §8.6). Gateway-side wall-clock
        // from dispatch to response headers; in production this INCLUDES real
        // upstream latency, so it is not pure loopback overhead — the Task 6.2
        // microbench (inline mock upstream) is the SLO gate (p99 <200µs UDS /
        // <500µs TCP). Label-free per spec §8.6. `.take()` records at most once.
        if let Some(sent) = ctx.egress_sent_at.take() {
            metrics::histogram!("gw_egress_loopback_rtt_seconds")
                .record(sent.elapsed().as_secs_f64());
        }
        use crate::ctx::ResponseMeta;
        ctx.archive_upstream_response_meta = Some(ResponseMeta {
            status: upstream_response.status.as_u16(),
            headers: upstream_response
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
                .collect(),
        });
        if let Some(meta) = ctx.archive_upstream_response_meta.as_ref() {
            struct QuotaHeaders<'a>(&'a [(String, String)]);
            impl crate::filters::response::QuotaHeaderView for QuotaHeaders<'_> {
                fn get(&self, name: &str) -> Option<&str> {
                    self.0
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(name))
                        .map(|(_, v)| v.as_str())
                }
            }
            ctx.quota_snapshot = crate::filters::response::QuotaSnapshotExtractor::extract(
                &QuotaHeaders(&meta.headers),
            );
        }

        // Capture response Content-Encoding before any strip. Cross-protocol
        // re-render emits plaintext; same-protocol transit reverse may
        // re-compress to a different byte length.
        ctx.resp_content_encoding = gw_codec::compression::parse_content_encoding(
            upstream_response
                .headers
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
        );
        let cross_protocol = match (ctx.client_codec.as_ref(), ctx.upstream_codec.as_ref()) {
            (Some(client), Some(upstream)) => {
                gw_codec::cross_render::WireFormat::from_any_codec(client)
                    != gw_codec::cross_render::WireFormat::from_any_codec(upstream)
            }
            _ => false,
        };
        if !ctx.is_stream && cross_protocol {
            let _ = upstream_response.remove_header("content-length");
            let _ = upstream_response.remove_header("content-encoding");
        } else if crate::filters::sensitive_filter::has_compressed_transit_response(ctx) {
            let _ = upstream_response.remove_header("content-length");
        }
        // ── Egress-impersonate loopback error mapping (Task 4.4) ──────────
        // The loopback egress (gw-egress-impersonate) emits a synthetic
        // 5xx/4xx carrying X-Egress-Error-Class / -Detail when the forged-TLS
        // dispatch to the real upstream fails. Map the class to a stable code,
        // capture the detail, then either short-circuit (tls block — a
        // different key won't beat a fingerprint block) or route through the
        // existing FailoverExecutor (transient transport faults). Always strip
        // the internal headers so the client never sees them.
        let egress_class: Option<String> = upstream_response
            .headers
            .get("x-egress-error-class")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned());
        if let Some(class) = egress_class.as_deref() {
            let (code, retryable, is_tls) = map_egress_class(class);
            ctx.upstream_error_code = Some(code);
            if let Some(detail) = upstream_response
                .headers
                .get("x-egress-error-detail")
                .and_then(|v| v.to_str().ok())
            {
                ctx.upstream_error_detail = Some(detail.to_owned());
            }
            // Strip internal headers before any client-facing path sees them.
            let _ = upstream_response.remove_header("x-egress-error-class");
            let _ = upstream_response.remove_header("x-egress-error-detail");

            let status = ctx.upstream_status.unwrap_or(502);

            if is_tls {
                // Fingerprint/TLS block — retrying a different key won't help.
                // Surface to the client; do not retry.
                metrics::counter!(
                    "gw_retry_attempt_total",
                    "outcome" => "tls_blocked",
                )
                .increment(1);
                let mut e = Error::new(ErrorType::HTTPStatus(status));
                e.as_up();
                return Err(e);
            }

            if retryable {
                if let Some(candidates) = ctx.picked_candidates.as_ref() {
                    let remaining = remaining_failover_candidates(candidates, ctx);
                    let attempt = ctx.failover_attempt.saturating_add(1);
                    let decision =
                        next_failover_decision(&self.failover_executor, &remaining, attempt, ctx);
                    match decision {
                        FailoverDecision::Retry(new_key) => {
                            // Audit 2026-05-30 §1.2: settle the prior attempt before
                            // mutating ctx for the next key. Bumps attempt_index (so
                            // the next inflight.register uses a fresh key instead of
                            // clobbering this attempt's) and emits its UsageRecord —
                            // build_attempt_record reads ctx.picked_vendor_key, so this
                            // MUST run before picked_vendor_key is overwritten below.
                            self.emit_response_surfaces(ctx, true).await;
                            crate::billing_lifecycle::settle_prior_attempt_spawn(
                                ctx,
                                self.inflight.clone(),
                                self.ledger.clone(),
                            );
                            if let Some(k) = ctx.picked_vendor_key.as_ref() {
                                if !ctx.retry_blacklist.iter().any(|id| id == &k.key_id) {
                                    ctx.retry_blacklist.push(k.key_id.clone());
                                }
                            }
                            metrics::counter!(
                                "gw_retry_attempt_total",
                                "outcome" => "egress_retry",
                            )
                            .increment(1);
                            ctx.failover_attempt = attempt;
                            reset_response_attempt_state(ctx, new_key);

                            let mut e = Error::new(ErrorType::HTTPStatus(status));
                            e.as_up();
                            e.set_retry(true);
                            return Err(e);
                        }
                        FailoverDecision::Timeout => {
                            ctx.upstream_error_code = Some(codes::GW_ROUTING_FAILOVER_TIMEOUT);
                            metrics::counter!(
                                "gw_routing_failover_total",
                                "reason" => "timeout",
                                "strategy" => ctx.strategy_name.unwrap_or("unknown").to_owned(),
                            )
                            .increment(1);
                        }
                        FailoverDecision::Exhausted | FailoverDecision::NotRetryable => {}
                    }
                    // Exhausted / NotRetryable → fall through to error classification.
                }
            }
            // Non-retryable, or retry candidates exhausted: fall through so the
            // status >= 400 error-classification block emits a client error body.
        }

        if let (None, Some(status)) = (egress_class.as_ref(), ctx.upstream_status) {
            if status == 429 {
                if let Some(key) = ctx.picked_vendor_key.as_ref() {
                    let rate_limit_kind =
                        detect_rate_limit_kind(&ResponseHeaderAccessor(upstream_response), None);
                    self.adaptive_rpm.observe_429_with_kind(
                        &key.key_id,
                        rate_limit_kind,
                        key.rpm_limit,
                        rpm_limit_header(upstream_response),
                        key.rpm_limit,
                        header_u32(upstream_response, "retry-after"),
                    );
                    if matches!(rate_limit_kind, RateLimitKind::Rpm)
                        || detect_rpm_kind(upstream_response)
                    {
                        metrics::counter!("gw_adaptive_rpm_rpm_detected_total").increment(1);
                    }
                }
            }
            match gw_routing::classify_for_failover(status, ctx.body_started, &self.retryable_codes)
            {
                class @ (FailoverClass::Retryable5xx
                | FailoverClass::Retryable429
                | FailoverClass::SwitchKey4xx) => {
                    if let Some(candidates) = ctx.picked_candidates.as_ref() {
                        let remaining = remaining_failover_candidates(candidates, ctx);
                        let attempt = ctx.failover_attempt.saturating_add(1);
                        let decision = next_failover_decision(
                            &self.failover_executor,
                            &remaining,
                            attempt,
                            ctx,
                        );
                        let strategy_name = ctx.strategy_name.unwrap_or("unknown");
                        gw_routing::failover::record_failover_metric(class, strategy_name);

                        match decision {
                            FailoverDecision::Retry(new_key) => {
                                // Audit 2026-05-30 §1.2: settle the prior attempt
                                // before mutating ctx (see egress arm above). Must
                                // run before picked_vendor_key is overwritten.
                                self.emit_response_surfaces(ctx, true).await;
                                crate::billing_lifecycle::settle_prior_attempt_spawn(
                                    ctx,
                                    self.inflight.clone(),
                                    self.ledger.clone(),
                                );
                                if let Some(k) = ctx.picked_vendor_key.as_ref() {
                                    if !ctx.retry_blacklist.iter().any(|id| id == &k.key_id) {
                                        ctx.retry_blacklist.push(k.key_id.clone());
                                    }
                                }
                                ctx.failover_attempt = attempt;
                                reset_response_attempt_state(ctx, new_key);

                                let mut e = Error::new(ErrorType::HTTPStatus(status));
                                e.as_up();
                                e.set_retry(true);
                                return Err(e);
                            }
                            FailoverDecision::Exhausted => {
                                metrics::counter!(
                                    "gw_routing_failover_total",
                                    "reason" => "exhausted",
                                    "strategy" => strategy_name.to_owned(),
                                )
                                .increment(1);
                            }
                            FailoverDecision::Timeout => {
                                ctx.upstream_error_code = Some(codes::GW_ROUTING_FAILOVER_TIMEOUT);
                                metrics::counter!(
                                    "gw_routing_failover_total",
                                    "reason" => "timeout",
                                    "strategy" => strategy_name.to_owned(),
                                )
                                .increment(1);
                            }
                            FailoverDecision::NotRetryable => {}
                        }
                    }
                }
                FailoverClass::BodyAlreadyStarted => {
                    let strategy_name = ctx.strategy_name.unwrap_or("unknown");
                    gw_routing::failover::record_failover_metric(
                        FailoverClass::BodyAlreadyStarted,
                        strategy_name,
                    );
                }
                FailoverClass::NotRetryable => {}
            }
        }

        if let Some(status) = ctx.upstream_status {
            if status >= 400 {
                if let Some(ref meta) = ctx.archive_upstream_response_meta {
                    let picked = ctx.picked_vendor_key.as_ref();
                    let classify_ctx = gw_core::error::ClassifyCtx {
                        key_id: picked.map_or("", |k| k.key_id.0.as_str()),
                        vendor_id: picked.map_or("", |k| k.vendor_id.0.as_str()),
                        api_format: ctx
                            .client_codec
                            .as_ref()
                            .map_or(gw_core::protocol::ProtocolKind::OpenAi, |c| c.kind()),
                        protocol: ctx
                            .upstream_codec
                            .as_ref()
                            .map_or(gw_core::protocol::ProtocolKind::OpenAi, |c| c.kind()),
                        has_oauth: picked.is_some_and(|k| k.oauth.is_some()),
                    };
                    struct MetaHeaders<'a>(&'a [(String, String)]);
                    impl gw_core::error::HeadersAccessor for MetaHeaders<'_> {
                        fn get(&self, name: &str) -> Option<&str> {
                            self.0
                                .iter()
                                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                                .map(|(_, v)| v.as_str())
                        }
                    }
                    let headers = MetaHeaders(&meta.headers);
                    let outcome =
                        self.classifier
                            .classify_upstream(&classify_ctx, status, &headers, None);
                    ctx.error_outcome = Some(outcome);

                    if self.error_passthrough_enabled {
                        ctx.error_passthrough_active = true;
                    }
                }
            }
        }

        Ok(())
    }

    fn upstream_response_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<Duration>> {
        if !ctx.is_stream || ctx.client_body_committed {
            return Ok(None);
        }

        let now = std::time::Instant::now();
        let started_at = *ctx.probe_started_at.get_or_insert(now);
        ctx.stream_phase = crate::stream_probe::StreamPhase::Probing;

        let timeout_state = crate::stream_probe::StreamProbeState {
            phase: ctx.stream_phase,
            client_committed: ctx.client_body_committed,
            started_at,
            timeout: ctx.probe_timeout,
            buffer: ctx.probe_buffer.clone(),
        };
        if crate::stream_probe::probe_timed_out(&timeout_state, now) {
            metrics::counter!("gw_stream_probe_timeout_total").increment(1);
            metrics::counter!("gw_stream_probe_total", "outcome" => "timeout").increment(1);
            let failure = crate::stream_probe::ProbeFailure::Timeout;
            if self.retry_stream_probe_failure(ctx, failure)? {
                let mut e = Error::new(ErrorType::HTTPStatus(504));
                e.as_up();
                e.set_retry(true);
                return Err(e);
            }
            return Ok(None);
        }

        let mut probe_state = crate::stream_probe::StreamProbeState {
            phase: ctx.stream_phase,
            client_committed: ctx.client_body_committed,
            started_at,
            timeout: ctx.probe_timeout,
            buffer: std::mem::take(&mut ctx.probe_buffer),
        };
        let decision = crate::stream_probe::apply_probe_chunk_with_codec(
            &mut probe_state,
            ctx.upstream_codec.as_ref(),
            body.take(),
            end_of_stream,
        );
        ctx.stream_phase = probe_state.phase;
        ctx.probe_buffer = probe_state.buffer;
        metrics::histogram!("gw_stream_probe_buffer_bytes").record(ctx.probe_buffer.len() as f64);

        match decision {
            crate::stream_probe::ProbeDecision::NeedMore => {
                *body = None;
                Ok(None)
            }
            crate::stream_probe::ProbeDecision::Commit(bytes) => {
                ctx.stream_phase = crate::stream_probe::StreamPhase::Streaming;
                *body = Some(bytes);
                metrics::counter!("gw_stream_probe_total", "outcome" => "commit").increment(1);
                Ok(None)
            }
            crate::stream_probe::ProbeDecision::Fail(failure) => {
                *body = None;
                metrics::counter!(
                    "gw_stream_probe_total",
                    "outcome" => failure.as_label(),
                )
                .increment(1);
                if self.retry_stream_probe_failure(ctx, failure)? {
                    let mut e =
                        Error::new(ErrorType::HTTPStatus(ctx.upstream_status.unwrap_or(502)));
                    e.as_up();
                    e.set_retry(true);
                    return Err(e);
                }
                Ok(None)
            }
            crate::stream_probe::ProbeDecision::AlreadyCommitted => Ok(None),
        }
    }

    fn response_body_filter(
        &self,
        session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<Duration>> {
        // W3-T3 / W3-T5: deadline check + grace window.
        //
        // 1. `check_response_deadline` fires `observe_cancel(Deadline)` on
        //    first exceedance; first-writer-wins semantics ensure it records
        //    exactly once.
        // 2. `check_grace_status` reads `cancel_observed_at_ms` (set by any
        //    cancel cause, not just Deadline) and decides:
        //    - NotApplicable → normal path
        //    - Active         → still in grace window; let this chunk through
        //                       so the extractor can catch a final usage frame
        //    - Expired        → grace window elapsed; record outcome + abort
        let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
        let _deadline_just_fired =
            crate::cancellation::check_response_deadline(ctx, self.response_deadline_ms, now_ms);

        match crate::cancellation::check_grace_status(ctx, self.cancel_grace_ms, now_ms) {
            crate::cancellation::GraceStatus::NotApplicable => {
                // Normal path — no cancel observed; continue below.
            }
            crate::cancellation::GraceStatus::Active => {
                // Within grace window — fall through to let the extractor see
                // this chunk; do not abort yet.
            }
            crate::cancellation::GraceStatus::Expired => {
                // Grace window elapsed. Heuristic: if VendorExposure reached
                // Settled the extractor saw a terminal frame during grace.
                let yielded = matches!(
                    ctx.vendor_exposure,
                    crate::billing_lifecycle::VendorExposure::Settled,
                );
                crate::cancellation::record_grace_outcome(yielded);
                return Err(Error::new(ErrorType::Custom("cancel_grace_expired")));
            }
        }

        let res = match ctx.archive_caps.storage {
            crate::ctx::ArchiveStorage::Memory => {
                filters::response::run_memory_sync(session, body, end_of_stream, ctx)
            }
            crate::ctx::ArchiveStorage::DiskSpill => tokio::task::block_in_place(|| {
                let rt = tokio::runtime::Handle::current();
                rt.block_on(filters::response::run(session, body, end_of_stream, ctx))
            }),
        };

        // W6-T07: classify_final_upstream_error + emit_response_surfaces moved
        // to the async `logging` filter to eliminate the blocking wrapper. The
        // error-passthrough re-render stays here because it must mutate `body`
        // before Pingora forwards the chunk; only its archive read is
        // conditional (sync for InMemory, blocking only for Spilled).

        if end_of_stream && ctx.error_passthrough_active {
            if let Some(status) = ctx.upstream_status {
                let client_kind = ctx
                    .client_codec
                    .as_ref()
                    .map_or(gw_core::protocol::ProtocolKind::OpenAi, |c| c.kind());

                // Read archive bytes for error-passthrough content detection.
                // W6-T07: InMemory / Truncated are read synchronously; only
                // Spilled needs the blocking wrapper for the async file read.
                // Fall back to empty on I/O error — the is_html / JSON parse
                // will simply fail open (no passthrough body rendered).
                let accumulated = match &ctx.archive_upstream_response_body {
                    crate::ctx::ArchiveState::InMemory(_)
                    | crate::ctx::ArchiveState::Truncated { .. } => {
                        ctx.archive_upstream_response_body.read_for_emit_sync()
                    }
                    crate::ctx::ArchiveState::Spilled { .. } => tokio::task::block_in_place(|| {
                        let rt = tokio::runtime::Handle::current();
                        rt.block_on(async {
                            match ctx.archive_upstream_response_body.read_for_emit().await {
                                Ok(b) => b,
                                Err(e) => {
                                    tracing::warn!(
                                        target: "gw_proxy::archive",
                                        request_id = %ctx.request_id,
                                        surface = "upstream",
                                        error = %e,
                                        "archive_emit_read_failed"
                                    );
                                    Vec::new()
                                }
                            }
                        })
                    }),
                };
                let is_html = accumulated.get(..100).is_some_and(|s| {
                    let start = std::str::from_utf8(s).unwrap_or("");
                    let trimmed = start.trim_start();
                    trimmed.starts_with("<!") || trimmed.starts_with("<html")
                });

                if !is_html {
                    if let Ok(body_json) = serde_json::from_slice::<serde_json::Value>(&accumulated)
                    {
                        let passthrough = gw_core::error::PassthroughBody {
                            upstream_status: status,
                            body_json,
                            is_sse: ctx.is_stream,
                        };
                        let error_class = ctx
                            .error_outcome
                            .as_ref()
                            .map_or(gw_core::error::ErrorClass::Internal, |o| o.class);
                        let derived_gateway_code = gw_codec::error_passthrough::derive_gateway_code(
                            &passthrough,
                            error_class,
                        );
                        let provider_id = ctx
                            .picked_vendor_key
                            .as_ref()
                            .map(|key| key.vendor_id.as_ref());
                        let model = ctx.model.as_ref().map(|model| model.as_ref());
                        let endpoint_id = ctx.selected_endpoint_id.as_deref();
                        let api_format = ctx.upstream_shape.map(|shape| shape.format_id());
                        let client_protocol =
                            ctx.client_shape.map(|shape| shape.format_id()).or_else(|| {
                                ctx.client_codec.as_ref().map(|codec| codec.kind().as_str())
                            });
                        let request_path = Some(session.req_header().uri.path());
                        let match_ctx = gw_core::error::message_rules::ErrorMessageMatchCtx {
                            provider_id,
                            model,
                            endpoint_id,
                            api_format,
                            request_path,
                            client_protocol,
                            http_status: ctx.upstream_status,
                            gateway_code: Some(derived_gateway_code.as_str()),
                        };
                        let snapshot = self.error_message_rules.snapshot();
                        let rendered =
                            gw_codec::error_passthrough::render_error_to_client_with_message_rules(
                                client_kind,
                                &passthrough,
                                error_class,
                                &self.error_passthrough_rules,
                                Some(&match_ctx),
                                Some(&snapshot),
                            );
                        *body = Some(rendered);
                    }
                }
            }
        }

        if res.is_ok() && body.as_ref().map(|b| !b.is_empty()).unwrap_or(false) {
            crate::stream_probe::mark_client_committed(ctx);
        }

        // W6-T07: emit_response_surfaces moved to the async `logging` filter
        // (runs after settle_actual + record_actual per M7 ordering invariant).
        res
    }

    /// G10 — cross-key retry on upstream connect failure.
    ///
    /// On any non-TLS connect error (timeout / refused / no-route / proxy
    /// failure / generic handshake) we push the failing `KeyId` onto
    /// `ctx.retry_blacklist`, bump `ctx.retry_count`, and call `e.set_retry(true)`.
    /// Pingora then calls `upstream_peer` again; `pick_peer` passes the blacklist
    /// into `get_candidate_keys`, so the next strategy pick is guaranteed to land
    /// on a different key (or `GW_ROUTING_NO_AVAILABLE_CHANNEL` if exhausted).
    ///
    /// TLS handshake / cert errors short-circuit immediately: a different vendor
    /// key against the same host won't fix a bad CA or expired cert, so we stamp
    /// `GW_UPSTREAM_TLS_ERROR` and let the error propagate to `fail_to_proxy`.
    ///
    /// When `retry_count` reaches `MAX_CONNECT_RETRIES` we stamp
    /// `GW_UPSTREAM_CONNECT_FAILED` and propagate. Metric:
    /// `gw_retry_attempt_total{outcome=recovered|exhausted|tls_blocked}`.
    ///
    /// Connect-only retry hook. Response-side (5xx / 429) retry is handled by
    /// `FailoverExecutor` in `response_filter` (see lines 515–556).
    fn fail_to_connect(
        &self,
        _session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        mut e: Box<Error>,
    ) -> Box<Error> {
        match classify_connect_error(&e) {
            ConnectErrorKind::Tls => {
                metrics::counter!(
                    "gw_retry_attempt_total",
                    "outcome" => "tls_blocked",
                )
                .increment(1);
                ctx.upstream_error_code = Some(codes::GW_UPSTREAM_TLS_ERROR);
                gw_core::observability::observe(gw_core::observability::ErrorObservation {
                    code: codes::GW_UPSTREAM_TLS_ERROR,
                    kind: gw_core::observability::ErrorKind::UpstreamConnect,
                    stage: gw_core::observability::Stage::Upstream,
                    status: None,
                    trace_id: Some(ctx.trace_id.as_ref()),
                    extra: None,
                });
                e
            }
            ConnectErrorKind::Connect => {
                invalidate_connect_failure_affinity(ctx, self.affinity.as_ref());
                if ctx.retry_count < MAX_CONNECT_RETRIES {
                    if let Some(k) = ctx.picked_vendor_key.as_ref().map(|p| p.key_id.clone()) {
                        ctx.retry_blacklist.push(k);
                    }
                    ctx.retry_count += 1;
                    // Drop the in-flight guard for the failed key so the next
                    // pick is not counted against the previous key's quota.
                    ctx.inflight_guard = None;

                    // Spec §5.6: emit a partial UsageRecord for the failed
                    // attempt and bump attempt_index before the next pick.
                    // `on_retry_sync` handles the synchronous state mutation
                    // and returns a record; the async inflight+ledger work is
                    // spawned on the current tokio runtime.
                    let attempt_record = crate::billing_lifecycle::on_retry_sync(ctx);
                    if let Some(rec) = attempt_record {
                        let inflight = self.inflight.clone();
                        let ledger = self.ledger.clone();
                        // old attempt_index is now ctx.attempt_index - 1 (bumped by on_retry_sync)
                        let old_attempt = ctx.attempt_index.saturating_sub(1);
                        let request_id = ctx.request_id.0.clone();
                        tokio::spawn(async move {
                            use gw_ledger::inflight::SettleState;
                            if let Err(e) = inflight
                                .mark_settled(&request_id, old_attempt, SettleState::Terminated)
                                .await
                            {
                                tracing::warn!(
                                    error = ?e,
                                    request_id = %request_id,
                                    attempt = old_attempt,
                                    "fail_to_connect on_retry: inflight mark_settled failed"
                                );
                            }
                            ledger
                                .emit(LedgerItem {
                                    record: rec,
                                    clear: Some(ClearKey {
                                        request_id: request_id.clone(),
                                        attempt_index: old_attempt,
                                    }),
                                })
                                .await;
                            // inflight.clear is NOT called here — see
                            // docs/spec/06-billing-ledger.md.
                            // If this spawn never runs (process shutdown), the inflight key remains
                            // and the startup reconciler emits a CrashFallback — A2 closed.
                        });
                    }

                    metrics::counter!(
                        "gw_retry_attempt_total",
                        "outcome" => "recovered",
                    )
                    .increment(1);
                    e.set_retry(true);
                    e
                } else {
                    metrics::counter!(
                        "gw_retry_attempt_total",
                        "outcome" => "exhausted",
                    )
                    .increment(1);
                    ctx.upstream_error_code = Some(codes::GW_UPSTREAM_CONNECT_FAILED);
                    gw_core::observability::observe(gw_core::observability::ErrorObservation {
                        code: codes::GW_UPSTREAM_CONNECT_FAILED,
                        kind: gw_core::observability::ErrorKind::UpstreamConnect,
                        stage: gw_core::observability::Stage::Upstream,
                        status: None,
                        trace_id: Some(ctx.trace_id.as_ref()),
                        extra: None,
                    });
                    e
                }
            }
        }
    }

    fn error_while_proxy(
        &self,
        peer: &HttpPeer,
        session: &mut Session,
        mut e: Box<Error>,
        ctx: &mut Self::CTX,
        client_reused: bool,
    ) -> Box<Error> {
        if ctx.stream_probe_retry_pending {
            ctx.stream_probe_retry_pending = false;
            e.set_retry(true);
            return e.more_context(format!("Peer: {peer}"));
        }
        let mut e = e.more_context(format!("Peer: {peer}"));
        e.retry
            .decide_reuse(client_reused && !session.as_ref().retry_buffer_truncated());
        e
    }

    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        e: &pingora::Error,
        ctx: &mut Self::CTX,
    ) -> pingora_proxy::FailToProxy {
        use pingora::ErrorSource;
        use pingora::ErrorType::*;
        use std::borrow::Cow;

        // If a prior filter (response_filter for egress-impersonate TLS, the
        // failover loop, etc.) already pinned a precise upstream error code
        // on `ctx`, honour it — the generic HTTPStatus → GW_UPSTREAM_PROTOCOL
        // tail below would otherwise mis-label TLS-fingerprint blocks and
        // failover-timeout / no-fallback-left exhaustion as "protocol error",
        // hiding actionable signal from the client envelope.
        let preset = ctx.upstream_error_code.and_then(|code| {
            let status = match e.etype() {
                HTTPStatus(c) => *c,
                _ => ctx.upstream_status.unwrap_or(502),
            };
            // Only short-circuit when we have a real HTTP status to surface;
            // a 0-status (e.g. ConnectionClosed before headers) keeps the
            // existing source-based mapping so non-envelope error reporting
            // still picks the right `ErrorKind`.
            if status == 0 {
                None
            } else {
                Some((code, status, kind_from_preset_code(code)))
            }
        });

        let (gw_code, http_status, kind): (&'static str, u16, gw_core::observability::ErrorKind) =
            if let Some(p) = preset {
                p
            } else {
                match e.etype() {
                    HTTPStatus(code) => (
                        codes::GW_UPSTREAM_PROTOCOL,
                        *code,
                        gw_core::observability::ErrorKind::UpstreamProtocol,
                    ),
                    _ => match e.esource() {
                        ErrorSource::Upstream => {
                            if ctx.body_started {
                                (
                                    codes::GW_UPSTREAM_TRUNCATED,
                                    0,
                                    gw_core::observability::ErrorKind::UpstreamMidStream,
                                )
                            } else {
                                (
                                    codes::GW_BACKEND_UNAVAILABLE,
                                    502,
                                    gw_core::observability::ErrorKind::UpstreamConnect,
                                )
                            }
                        }
                        ErrorSource::Downstream => match e.etype() {
                            WriteError | ReadError | ConnectionClosed => (
                                codes::GW_CLIENT_DISCONNECTED,
                                0,
                                // No DownstreamDisconnect kind; UpstreamConnect is the closest available.
                                gw_core::observability::ErrorKind::UpstreamConnect,
                            ),
                            _ => (
                                codes::GW_BAD_REQUEST,
                                400,
                                gw_core::observability::ErrorKind::UpstreamConnect,
                            ),
                        },
                        ErrorSource::Internal | ErrorSource::Unset => (
                            codes::GW_GATEWAY_INTERNAL,
                            500,
                            gw_core::observability::ErrorKind::UpstreamConnect,
                        ),
                    },
                }
            };

        let category = match gw_code {
            codes::GW_BAD_REQUEST => gw_core::error::ErrorCategory::ClientBadRequest,
            codes::GW_GATEWAY_INTERNAL => gw_core::error::ErrorCategory::GatewayInternal,
            _ => gw_core::error::ErrorCategory::UpstreamError,
        };

        // TLS fingerprint / OAuth-credential blocks won't be helped by a retry;
        // failover-timeout / no-fallback-left already exhausted alternatives.
        // Everything else keeps the legacy 5xx-only retryable heuristic.
        let retryable = match gw_code {
            codes::GW_UPSTREAM_TLS_ERROR
            | codes::GW_ROUTING_FAILOVER_TIMEOUT
            | codes::GW_ROUTING_NO_FALLBACK_LEFT
            | codes::GW_UPSTREAM_CONNECT_FAILED => false,
            _ => matches!(http_status, 502..=504),
        };

        let message: Cow<'static, str> = match gw_code {
            codes::GW_BACKEND_UNAVAILABLE => Cow::Borrowed("upstream unavailable"),
            codes::GW_UPSTREAM_PROTOCOL => Cow::Borrowed("upstream protocol error"),
            codes::GW_UPSTREAM_TLS_ERROR => {
                Cow::Borrowed("upstream TLS handshake or fingerprint rejected")
            }
            codes::GW_UPSTREAM_CONNECT_FAILED => Cow::Borrowed("upstream connect failed"),
            codes::GW_ROUTING_FAILOVER_TIMEOUT => Cow::Borrowed("failover timeout exhausted"),
            codes::GW_ROUTING_NO_FALLBACK_LEFT => Cow::Borrowed("no fallback candidate left"),
            codes::GW_BAD_REQUEST => Cow::Borrowed("bad request"),
            codes::GW_GATEWAY_INTERNAL => Cow::Borrowed("gateway internal error"),
            _ => Cow::Borrowed("upstream unavailable"),
        };

        gw_core::observability::observe(gw_core::observability::ErrorObservation {
            code: gw_code,
            kind,
            stage: gw_core::observability::Stage::Upstream,
            status: if http_status > 0 {
                Some(http_status)
            } else {
                None
            },
            trace_id: Some(ctx.trace_id.as_ref()),
            extra: None,
        });

        if http_status > 0 && !ctx.body_started {
            let envelope = gw_core::error::ErrorIR {
                category,
                status: http_status,
                upstream_status: None,
                code: Cow::Borrowed(gw_code),
                message,
                retryable,
                upstream_raw: None,
                request_id: ctx.request_id.as_ref(),
                retry_after_secs: None,
            };
            if let Ok(body) = serde_json::to_vec(&envelope) {
                let mut resp = pingora::http::ResponseHeader::build(http_status, None)
                    .unwrap_or_else(|_| pingora::http::ResponseHeader::build(500, None).unwrap());
                let _ = resp.insert_header("content-type", "application/json");
                let _ = resp.insert_header("content-length", body.len().to_string());
                let _ = session.write_response_header(Box::new(resp), false).await;
                let _ = session
                    .write_response_body(Some(bytes::Bytes::from(body)), true)
                    .await;
            }
        }

        pingora_proxy::FailToProxy {
            error_code: http_status,
            can_reuse_downstream: false,
        }
    }

    async fn logging(
        &self,
        session: &mut Session,
        err: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        // W3-T2/T4: classify pingora errors for cancel attribution.
        // Checks for both ClientClose (Downstream) and UpstreamErr (Upstream).
        if let Some(e) = err {
            use pingora::ErrorSource::*;
            use pingora::ErrorType::*;
            let is_relevant_etype = matches!(e.etype, ConnectionClosed | WriteError | ReadError);
            let is_upstream = is_relevant_etype && matches!(e.esource, Upstream);
            let is_client_close = is_relevant_etype && matches!(e.esource, Downstream);
            if let Some(cause) =
                crate::cancellation::pingora_error_to_cancel_cause(is_upstream, is_client_close)
            {
                crate::cancellation::observe_cancel(ctx, cause);
            }
        }

        // W6-T07: classify_final_upstream_error moved here from
        // response_body_filter. Runs BEFORE filters::logging::run so that
        // ctx.error_outcome (set by this call) is fresh when the error-handler
        // fanout inside filters::logging::run reads it. This is async (reads
        // archive via read_for_emit) — the async logging trait method can
        // await it directly without a blocking wrapper.
        self.classify_final_upstream_error(ctx).await;

        filters::logging::run(
            session,
            err,
            ctx,
            &self.health,
            &self.ledger,
            &self.counters,
            &self.store,
            &self.resilience,
            self.affinity.as_ref(),
            &self.ratelimit,
            self.quota_state_updates.as_ref(),
            &self.overflow_gate,
        )
        .await;

        // W6-T01 + W6-T07: emit_response_surfaces moved here from
        // response_body_filter. Runs AFTER filters::logging::run which
        // performs guard.settle_actual(actual) → overflow_gate.record_actual(...)
        // (filters/logging.rs:160-172). M7 ordering invariant: settle_actual
        // → record_actual → emit_response_surfaces — must not be reversed or
        // interleaved (would double-count overflow or race settle refund with
        // overflow INCRBY).
        //
        // W6-T01: force_flush_partial=true for grace-expired requests (cancel
        // observed + grace window elapsed). The signal reuses existing ctx
        // state: cancel_observed_at_ms.is_some() && !cancel_grace_active
        // uniquely identifies the GraceStatus::Expired path (the Expired arm
        // is the only branch that clears cancel_grace_active while
        // cancel_observed_at_ms remains Some). No new ctx field needed.
        let force_flush_partial = ctx.cancel_observed_at_ms.is_some() && !ctx.cancel_grace_active;
        self.emit_response_surfaces(ctx, force_flush_partial).await;
        if force_flush_partial {
            metrics::counter!(ARCHIVE_EMIT_GRACE_FLUSH_TOTAL).increment(1);
        }

        // mark_settled after the UsageRecord has been sent to the ledger.
        // If the request never reached the upstream (short-circuit before
        // rewrite_request), `picked_vendor_key` will be None and there is
        // no inflight hash to update — so we gate on vendor_key being set.
        if ctx.picked_vendor_key.is_some() {
            use crate::billing_lifecycle::VendorExposure;
            use gw_ledger::inflight::SettleState;
            let settle_state = match ctx.vendor_exposure {
                VendorExposure::Settled => SettleState::Settled,
                _ => SettleState::Terminated,
            };
            if let Err(e) = self
                .inflight
                .mark_settled(&ctx.request_id.0, ctx.attempt_index, settle_state)
                .await
            {
                tracing::warn!(
                    error = ?e,
                    request_id = %ctx.request_id,
                    "inflight mark_settled failed"
                );
            }
            // inflight.clear is NOT called here — see
            // docs/spec/06-billing-ledger.md.
            // The flusher DELs the key after XADD-confirmed delivery (invariant I1).
        }

        // Emit summary surface (always, including error paths). Bodies were
        // emitted at their respective trigger points; this entry carries the
        // tabular fields previously stored on DetailedLogRecord.
        {
            use gw_ledger::detailed_archive::{
                CapturedSurface, Kind, SummaryPayload, SurfacePayload,
            };
            let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
            let latency_ms = match (ctx.timing.start, ctx.timing.end) {
                (Some(s), Some(e)) => e
                    .saturating_duration_since(s)
                    .as_millis()
                    .min(u32::MAX as u128) as u32,
                _ => 0,
            };
            let ttft_ms = match (ctx.timing.start, ctx.timing.ttft) {
                (Some(s), Some(t)) => Some(
                    t.saturating_duration_since(s)
                        .as_millis()
                        .min(u32::MAX as u128) as u32,
                ),
                _ => None,
            };
            let summary = SummaryPayload {
                user_id: ctx.effective_user_id().cloned(),
                api_key_id: ctx.api_key_id.clone(),
                model: ctx.model.clone(),
                client_protocol: ctx
                    .client_shape
                    .map(|s| s.format_id().to_owned())
                    .or_else(|| {
                        ctx.client_codec
                            .as_ref()
                            .map(|c| c.kind().as_str().to_owned())
                    }),
                upstream_protocol: ctx
                    .upstream_shape
                    .map(|s| s.format_id().to_owned())
                    .or_else(|| {
                        ctx.upstream_codec
                            .as_ref()
                            .map(|c| c.kind().as_str().to_owned())
                    }),
                http_status: ctx.upstream_status.unwrap_or(0),
                error_code: err.map(|e| format!("{:?}", e.etype)),
                is_stream: ctx.is_stream,
                input_tokens: ctx.usage.input_tokens,
                output_tokens: ctx.usage.output_tokens,
                latency_ms,
                ttft_ms,
                tunnel_resolution: ctx.tunnel_resolution.clone(),
            };
            let summary = mask_archive_summary(ctx.sensitive_filter_policy.as_deref(), summary);
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: now_ms,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                attempt_index: ctx.attempt_index,
                kind: Kind::Summary,
                payload: SurfacePayload::Summary(summary),
            });
        }
    }

    /// Silence pingora's own ERROR-level log for the gateway's intentional
    /// short-circuits.
    ///
    /// By the time `suppress_error_log` runs, the client has already
    /// received a vendor-flavoured JSON envelope via
    /// `error_emit::send_error`, so the `error!("Fail to filter request"
    /// / "Fail to proxy")` line in `pingora_proxy::lib.rs` is
    /// double-counting an intentional rejection. We mute only that exact
    /// shape — any other error (upstream connect refused, upstream
    /// timeout, an internal sanity-check that bubbles a bare `HTTPStatus`
    /// without our marker cause) still falls through to the default
    /// `false` and is logged at ERROR.
    ///
    /// The `logging` filter still receives the same `Option<&Error>` and
    /// keeps `gateway_requests_total{status_class}` partitioning 4xx /
    /// 5xx, so observability does not regress.
    ///
    /// The actual shape check lives in [`is_gw_short_circuit`] so it can
    /// be unit-tested without standing up a `Session`.
    fn suppress_error_log(
        &self,
        _session: &Session,
        _ctx: &Self::CTX,
        error: &pingora::Error,
    ) -> bool {
        is_gw_short_circuit(error)
    }
}

/// Returns true when the error came out of `filters::early::short_circuit`.
///
/// The short-circuit builder stamps an outer `ErrorType::HTTPStatus(_)`
/// on top of a root cause of `ErrorType::Custom("gw_short_circuit")`. Both
/// conditions must hold so genuine pingora failures (connect refused,
/// upstream timeout, sanity-check 500s without the marker cause) still
/// surface at ERROR.
fn is_gw_short_circuit(error: &pingora::Error) -> bool {
    matches!(error.etype, ErrorType::HTTPStatus(_))
        && matches!(
            error.root_etype(),
            ErrorType::Custom(s) if *s == "gw_short_circuit"
        )
}

/// 16-byte random ID encoded as lowercase hex (32 chars).
fn trace_id_hex() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::{
        adapter_body_head, classify_connect_error, invalidate_connect_failure_affinity,
        is_gw_short_circuit, merge_adapter_error_outcome, next_failover_decision, ConnectErrorKind,
    };
    use bytes::BytesMut;
    use gw_adapter::{ErrorKind, ErrorOutcome};
    use gw_core::entity::{KeyStatus, VendorKey};
    use gw_core::error::{ClassifierOutcome, CooldownDirective, ErrorClass, Retryability};
    use gw_core::ids::{ApiKeyId, KeyId, VendorId};
    use gw_core::ratelimit::RateLimitKind;
    use gw_routing::affinity::{AffinityDimension, AffinityManager};
    use gw_routing::{FailoverDecision, FailoverExecutor};
    use pingora::{Error, ErrorType};
    use secrecy::SecretString;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn make_test_key(key_id: &str) -> Arc<VendorKey> {
        Arc::new(VendorKey {
            key_id: KeyId::from(key_id),
            vendor_id: VendorId::from("vendor-test"),
            api_key: SecretString::from("sk-test".to_string()),
            accessible_models: vec![],
            model_mapping: Default::default(),
            model_cost_weights: Default::default(),
            accessible_endpoints: vec![],
            protocol_conversion: false,
            priority: 0,
            weight: 1,
            rpm_limit: 0,
            tpm_limit: 0,
            max_usage_count: None,
            proxy: None,
            ip_allowlist: None,
            expires_at: None,
            status: KeyStatus::Active,
            tags: vec![],
            forced_capabilities: vec![],
            exclusive_capability_match: false,
            tunnel_preference: None,
            oauth: None,
            identity_snapshot: None,
            quota_state: Default::default(),
        })
    }

    #[test]
    fn classify_connect_error_tls_outer() {
        let e = Error::new(ErrorType::TLSHandshakeFailure);
        assert_eq!(classify_connect_error(&e), ConnectErrorKind::Tls);
    }

    #[test]
    fn classify_connect_error_tls_in_root_cause() {
        // ConnectError wrapping a TLS failure → still classified TLS.
        let e = Error::because(
            ErrorType::ConnectError,
            "wrapped",
            Error::new(ErrorType::InvalidCert),
        );
        assert_eq!(classify_connect_error(&e), ConnectErrorKind::Tls);
    }

    #[test]
    fn classify_connect_error_plain_connect() {
        let e = Error::new(ErrorType::ConnectTimedout);
        assert_eq!(classify_connect_error(&e), ConnectErrorKind::Connect);
        let e = Error::new(ErrorType::ConnectRefused);
        assert_eq!(classify_connect_error(&e), ConnectErrorKind::Connect);
        let e = Error::new(ErrorType::ConnectNoRoute);
        assert_eq!(classify_connect_error(&e), ConnectErrorKind::Connect);
    }

    #[test]
    fn next_failover_decision_honors_request_elapsed_budget() {
        let mut ctx = crate::ctx::ReqCtx::default();
        ctx.timing.start = Some(Instant::now() - Duration::from_millis(25));
        let executor = FailoverExecutor::with_total_budget_ms(3, 10);
        let remaining = vec![make_test_key("key-a")];

        assert!(matches!(
            next_failover_decision(&executor, &remaining, 1, &ctx),
            FailoverDecision::Timeout
        ));
    }

    #[test]
    fn connect_failure_invalidates_affinity_for_picked_vendor_key() {
        let affinity = AffinityManager::default();
        let picked = make_test_key("vk-connect-failed");
        let ctx = crate::ctx::ReqCtx {
            api_key_id: Some(ApiKeyId::from("ak-connect")),
            picked_vendor_key: Some(picked.clone()),
            ..crate::ctx::ReqCtx::default()
        };
        affinity.record_hit(
            AffinityDimension::ApiKey(ApiKeyId::from("ak-connect")),
            picked.key_id.clone(),
            Instant::now(),
        );

        invalidate_connect_failure_affinity(&ctx, &affinity);

        assert!(affinity
            .get(
                &AffinityDimension::ApiKey(ApiKeyId::from("ak-connect")),
                Instant::now(),
            )
            .is_none());
    }

    #[test]
    fn suppresses_gateway_short_circuit() {
        let e = Error::because(
            ErrorType::HTTPStatus(401),
            "gateway short-circuit",
            Error::new(ErrorType::Custom("gw_short_circuit")),
        );
        assert!(is_gw_short_circuit(&e));
    }

    #[test]
    fn does_not_suppress_bare_http_status() {
        // Outer HTTPStatus but no marker cause — should still log.
        let e = Error::new(ErrorType::HTTPStatus(500));
        assert!(!is_gw_short_circuit(&e));
    }

    #[test]
    fn does_not_suppress_upstream_failure() {
        // No HTTPStatus envelope; root cause is the marker but outer
        // etype is a genuine pingora failure — should still log.
        let e = Error::because(
            ErrorType::ConnectRefused,
            "upstream down",
            Error::new(ErrorType::Custom("gw_short_circuit")),
        );
        assert!(!is_gw_short_circuit(&e));
    }

    #[test]
    fn egress_class_mapping_is_stable() {
        use super::map_egress_class;
        use gw_core::error::codes::*;
        assert_eq!(
            map_egress_class("tls"),
            (GW_UPSTREAM_TLS_ERROR, false, true)
        );
        assert_eq!(
            map_egress_class("connect_timeout"),
            (GW_UPSTREAM_CONNECT_TIMEOUT, true, false)
        );
        assert_eq!(
            map_egress_class("connect_refused"),
            (GW_UPSTREAM_CONNECT_REFUSED, true, false)
        );
        assert_eq!(
            map_egress_class("read_timeout"),
            (GW_UPSTREAM_READ_TIMEOUT, true, false)
        );
        assert_eq!(
            map_egress_class("h2_protocol"),
            (GW_UPSTREAM_H2_PROTOCOL, true, false)
        );
        assert_eq!(
            map_egress_class("body_truncated"),
            (GW_UPSTREAM_BODY_TRUNCATED, false, false)
        );
        assert_eq!(
            map_egress_class("proxy_error"),
            (GW_UPSTREAM_PROXY_ERROR, true, false)
        );
        assert_eq!(map_egress_class("panic"), (GW_UPSTREAM_PANIC, false, false));
        assert_eq!(
            map_egress_class("something_else"),
            (GW_UPSTREAM_UNKNOWN, false, false)
        );
    }

    #[test]
    fn redact_loopback_headers_strips_internal_and_keeps_rest() {
        let input = vec![
            (
                "X-Egress-Target".to_string(),
                "api.example.com:443".to_string(),
            ),
            ("x-egress-profile".to_string(), "chrome".to_string()),
            ("X-EGRESS-PROXY".to_string(), "http://p".to_string()),
            ("x-egress-error-class".to_string(), "tls".to_string()),
            (
                "X-Egress-Error-Detail".to_string(),
                "handshake failed".to_string(),
            ),
            ("X-Gateway-Tunnel-Node-Id".to_string(), "n1".to_string()),
            (
                "x-gateway-tunnel-target".to_string(),
                "api.example.com:443".to_string(),
            ),
            (
                "X-Gateway-Tunnel-Target-Tls".to_string(),
                "true".to_string(),
            ),
            ("authorization".to_string(), "Bearer secret".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let out = super::redact_loopback_headers(input);
        let names: Vec<String> = out.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
        // All loopback-internal headers (any case) are gone.
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("x-egress-") || n.starts_with("x-gateway-tunnel-")),
            "loopback headers leaked: {names:?}"
        );
        // Non-loopback headers are preserved, in order.
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "authorization");
        assert_eq!(out[1].0, "Content-Type");
    }

    #[test]
    fn archive_mask_rewrites_header_values_and_body() {
        let mut ctx = crate::ctx::ReqCtx::default();
        ctx.sensitive_filter_policy = Some(Arc::new(
            crate::filters::sensitive_filter::test_support::archive_policy(),
        ));
        let headers = vec![
            ("authorization".to_string(), "Bearer ak-secret".to_string()),
            ("x-client".to_string(), "keep".to_string()),
        ];
        let body = Arc::new(bytes::Bytes::from_static(b"{\"key\":\"ak-secret\"}"));

        let (headers, body, dropped) = super::mask_archive_surface(&ctx, headers, body);
        let url = super::mask_archive_text(
            ctx.sensitive_filter_policy.as_deref(),
            "/v1/chat?api_key=ak-secret".to_string(),
        );

        assert_eq!(
            headers,
            vec![
                ("authorization".to_string(), "Bearer <AK>".to_string()),
                ("x-client".to_string(), "keep".to_string()),
            ]
        );
        assert_eq!(body.as_ref().as_ref(), b"{\"key\":\"<AK>\"}");
        assert_eq!(dropped, 0);
        assert_eq!(url, "/v1/chat?api_key=<AK>");
    }

    #[test]
    fn archive_mask_rewrites_compressed_body_payloads() {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;

        let mut ctx = crate::ctx::ReqCtx::default();
        ctx.sensitive_filter_policy = Some(Arc::new(
            crate::filters::sensitive_filter::test_support::archive_policy(),
        ));
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"{\"key\":\"ak-secret\"}").unwrap();
        let compressed = encoder.finish().unwrap();

        let (body, dropped) = super::mask_archive_body_with_encoding(
            ctx.sensitive_filter_policy.as_deref(),
            Arc::new(bytes::Bytes::from(compressed)),
            super::ArchiveBodyEncoding::Supported(gw_codec::compression::Encoding::Gzip),
        );

        let mut decoder = flate2::read::GzDecoder::new(body.as_ref().as_ref());
        let mut decoded = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut decoded).unwrap();
        assert_eq!(decoded, "{\"key\":\"<AK>\"}");
        assert_eq!(dropped, 0);
    }

    #[test]
    fn archive_mask_drops_unreadable_compressed_body_payloads() {
        let policy = Arc::new(crate::filters::sensitive_filter::test_support::archive_policy());
        let raw = Arc::new(bytes::Bytes::from_static(b"not gzip ak-secret"));

        let (body, dropped) = super::mask_archive_body_with_encoding(
            Some(policy.as_ref()),
            raw,
            super::ArchiveBodyEncoding::Supported(gw_codec::compression::Encoding::Gzip),
        );

        assert!(body.is_empty());
        assert_eq!(dropped, b"not gzip ak-secret".len() as u64);
    }

    #[test]
    fn archive_mask_drops_unsupported_encoded_body_payloads() {
        let policy = Arc::new(crate::filters::sensitive_filter::test_support::archive_policy());
        let headers = vec![("content-encoding".to_string(), "gzip, br".to_string())];
        let raw = Arc::new(bytes::Bytes::from_static(b"compressed ak-secret"));
        let mut ctx = crate::ctx::ReqCtx::default();
        ctx.sensitive_filter_policy = Some(policy);

        let (_, body, dropped) = super::mask_archive_surface(&ctx, headers, raw);

        assert!(body.is_empty());
        assert_eq!(dropped, b"compressed ak-secret".len() as u64);
    }

    #[test]
    fn archive_mask_rewrites_summary_strings() {
        let policy = Arc::new(crate::filters::sensitive_filter::test_support::archive_policy());
        let summary = gw_ledger::detailed_archive::SummaryPayload {
            user_id: Some(gw_core::ids::UserId::from("ak-user")),
            api_key_id: Some(gw_core::ids::ApiKeyId::from("ak-key")),
            model: Some(gw_core::ids::ModelName::from("model-ak-secret")),
            client_protocol: Some("openai-ak-secret".to_string()),
            upstream_protocol: Some("anthropic-ak-secret".to_string()),
            http_status: 200,
            error_code: Some("ak-error".to_string()),
            is_stream: false,
            input_tokens: 1,
            output_tokens: 2,
            latency_ms: 3,
            ttft_ms: Some(4),
            tunnel_resolution: Some("direct-ak-secret".to_string()),
        };

        let masked = super::mask_archive_summary(Some(policy.as_ref()), summary);

        assert_eq!(masked.user_id.unwrap().as_ref(), "<AK>");
        assert_eq!(masked.api_key_id.unwrap().as_ref(), "<AK>");
        assert_eq!(masked.model.unwrap().as_ref(), "model-<AK>");
        assert_eq!(masked.client_protocol.as_deref(), Some("openai-<AK>"));
        assert_eq!(masked.upstream_protocol.as_deref(), Some("anthropic-<AK>"));
        assert_eq!(masked.error_code.as_deref(), Some("<AK>"));
        assert_eq!(masked.tunnel_resolution.as_deref(), Some("direct-<AK>"));
        assert_eq!(masked.http_status, 200);
        assert_eq!(masked.input_tokens, 1);
    }

    #[test]
    fn analyzer_cooldown_overrides_default_classifier_cooldown() {
        let default = ClassifierOutcome {
            class: ErrorClass::RateLimitedUpstream(RateLimitKind::Unknown),
            retryable: Retryability::CrossKey,
            cooldown: Some(CooldownDirective {
                reason_code: 7,
                ttl_secs: 60,
                reset_consecutive: false,
            }),
            ..Default::default()
        };
        let analyzer = ErrorOutcome {
            kind: Some(ErrorKind::Quota),
            retry_after_secs: Some(17),
            cooldown: Some(CooldownDirective {
                reason_code: 7,
                ttl_secs: 17,
                reset_consecutive: false,
            }),
            ..Default::default()
        };
        let merged = merge_adapter_error_outcome(default, analyzer);
        assert_eq!(merged.cooldown.as_ref().unwrap().ttl_secs, 17);
    }

    #[test]
    fn body_head_is_capped_to_8k() {
        let body = BytesMut::from(&vec![b'a'; 9000][..]);
        let head = adapter_body_head(&body);
        assert_eq!(head.len(), 8192);
    }
}
