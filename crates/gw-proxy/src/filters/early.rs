//! `early_request_filter` — first filter to run; pre-auth checks.
//!
//! Responsibilities (ADR-001 / ADR-014):
//! 1. Stamp `ctx.trace_id` into a `traceparent`-ish header so downstream logs
//!    correlate (we keep the auto-generated id from `new_ctx`; W3C
//!    trace-context parsing is not yet implemented).
//! 2. Run the unauthenticated portion of the IP blocklist (`Global` scope
//!    entries only — scoped entries are re-checked in `request_filter` once
//!    the user/api_key resolves).
//! 3. Enforce the *global* concurrent-stream cap (`max_concurrent_streams`
//!    via the shared `RateLimitGuard.total_streams`). The per-user cap
//!    happens in `request_filter`.
//! 4. Detect the client wire protocol with `AnyCodec::detect`; cache the
//!    selected codec on `ctx.client_codec`. Failed detection short-circuits
//!    with `GW_REQUEST_PROTOCOL_UNKNOWN`.
//! 5. (Skeleton) Debug-trigger check: header `X-Gw-Debug: 1` or a
//!    trace-suffix marker promotes `ctx.debug` to enabled.

use crate::ctx::ReqCtx;
use crate::error_emit;
use gw_auth::ip_filter::check_blocklist;
use gw_auth::ratelimit::RateLimitGuard;
use gw_codec::trait_def::Headers;
use gw_codec::AnyCodec;
use gw_core::debug::DebugTrigger;
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_store::EntityStore;
use pingora::{Error, ErrorType, Result};
use pingora_proxy::Session;
use std::borrow::Cow;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Returned to the gateway as a short-circuit signal when we've already
/// written the error response and just want the filter chain to abort. We
/// borrow `pingora::Error::HTTPStatus` to surface the right wire status to
/// downstream Pingora bookkeeping.
pub(crate) fn short_circuit(status: u16) -> Box<Error> {
    Error::because(
        ErrorType::HTTPStatus(status),
        "gateway short-circuit",
        Error::new(ErrorType::Custom("gw_short_circuit")),
    )
}

/// Hard ceiling on globally-inflight streaming requests. v1: 100k. Moving this
/// to config is tracked under phase-5.
const GLOBAL_STREAM_CAP: usize = 100_000;

pub async fn run(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    ratelimit: &Arc<RateLimitGuard>,
    trust_xff: bool,
) -> Result<()> {
    ctx.phase_start("early_request_filter");
    // Snapshot the protocol detection inputs so we don't borrow the session
    // mutably across multiple immutable reads.
    let path = session.req_header().uri.path().to_string();
    let headers_vec: Vec<(String, String)> = session
        .req_header()
        .headers
        .iter()
        .map(|(n, v)| (n.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect();
    let request_id = ctx.request_id.0.clone();

    // -----------------------------------------------------------------
    // 1. IP blocklist (Global scope only — User/ApiKey scoping happens
    //    in `request_filter` once auth resolves the IDs).
    // -----------------------------------------------------------------
    if let Some(ip) = client_ip_from_session(session, trust_xff) {
        if let Err(err) = check_blocklist(ip, None, None, store).await {
            let codec = pick_emit_codec(&ctx.client_codec);
            let scoped = with_request_id(&err, &request_id);
            error_emit::send_error(session, &codec, &scoped).await;
            ctx.phase_end();
            return Err(short_circuit(err.status));
        }
    }

    // -----------------------------------------------------------------
    // 3. Global concurrent-stream cap. The per-user cap is enforced in
    //    `request_filter`; here we protect the whole process against
    //    runaway concurrency before we spend any auth/parse cycles.
    //
    //    Note: ordering — we accept that a request can briefly exceed the
    //    cap if many slots open concurrently before any has registered;
    //    the drop is bounded because per-user `check_stream_slot` is the
    //    only place the counter increments and is gated downstream.
    // -----------------------------------------------------------------
    let total_inflight = ratelimit.total_streams.load(Ordering::Acquire);
    if total_inflight >= GLOBAL_STREAM_CAP {
        let err = ErrorIR {
            category: ErrorCategory::ClientQuota,
            status: 429,
            upstream_status: None,
            code: Cow::Borrowed(codes::GW_QUOTA_CONCURRENT_EXCEEDED),
            message: Cow::Borrowed("global concurrent stream cap exceeded"),
            retryable: true,
            upstream_raw: None,
            request_id: request_id.as_str(),
            retry_after_secs: Some(5),
        };
        let codec = pick_emit_codec(&ctx.client_codec);
        error_emit::send_error(session, &codec, &err).await;
        ctx.phase_end();
        return Err(short_circuit(err.status));
    }

    // -----------------------------------------------------------------
    // 4. Protocol detection — must happen before we can render any error
    //    in the caller's wire format.
    // -----------------------------------------------------------------
    let head_slice: &Headers = &headers_vec;
    let codec = match AnyCodec::detect(&path, head_slice, &[]) {
        Some(c) => c,
        None => {
            let err = ErrorIR {
                category: ErrorCategory::ClientBadRequest,
                status: 400,
                upstream_status: None,
                code: Cow::Borrowed(codes::GW_REQUEST_PROTOCOL_UNKNOWN),
                message: Cow::Borrowed("unknown protocol"),
                retryable: false,
                upstream_raw: None,
                request_id: request_id.as_str(),
                retry_after_secs: None,
            };
            // Fall back to OpenAI-shaped error response since we have no
            // codec preference yet.
            let fallback = AnyCodec::OpenAi(gw_codec::openai::OpenAi);
            error_emit::send_error(session, &fallback, &err).await;
            ctx.phase_end();
            return Err(short_circuit(err.status));
        }
    };
    ctx.client_codec = Some(codec);

    // -----------------------------------------------------------------
    // 5. Debug trigger (header opt-in). The full sampling / trace-suffix
    //    logic lives in gw-gateway; here we only honour the explicit
    //    header switch.
    // -----------------------------------------------------------------
    if header_lookup(&headers_vec, "x-gw-debug").is_some_and(|v| v == "1") {
        ctx.debug.upgrade_to_enabled(DebugTrigger::Header);
    }

    // -----------------------------------------------------------------
    // 6. ADR-S3: sticky-strategy binding key. Header is case-insensitive
    //    per HTTP semantics; absent → `Sticky` falls back to
    //    "first available" (strictly additive — identical to v1 behaviour).
    // -----------------------------------------------------------------
    ctx.conversation_id = header_lookup(&headers_vec, "x-conversation-id")
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned());

    ctx.phase_end();
    Ok(())
}

/// Resolve the client IP for blocklist / permission checks.
///
/// When `trust_xff` is `true`, prefer the first (left-most) entry of the
/// `X-Forwarded-For` header, which per RFC 7239 convention is the original
/// client as seen by the trusted L7 load balancer. Operators MUST only
/// enable this flag behind a load balancer they own end-to-end, otherwise
/// clients can forge the header and trivially bypass the IP blocklist.
///
/// When `trust_xff` is `false` (the default), use Pingora's `client_addr()`
/// — i.e. the L4 socket peer. Behind a reverse proxy this returns the
/// proxy's IP, so the IP allow / blocklist matches against the proxy
/// rather than the client; that's a safe fail-closed default while we
/// confirm the deployment topology.
///
/// Returns `None` for non-Inet sockets (e.g. Unix domain sockets in tests)
/// or unparseable XFF values.
pub(crate) fn client_ip_from_session(session: &Session, trust_xff: bool) -> Option<IpAddr> {
    if trust_xff {
        if let Some(xff) = session.req_header().headers.get("x-forwarded-for") {
            if let Ok(s) = xff.to_str() {
                if let Some(first) = s.split(',').next() {
                    if let Ok(ip) = first.trim().parse::<IpAddr>() {
                        return Some(ip);
                    }
                }
            }
        }
    }
    session.client_addr().and_then(|sa| match sa {
        pingora::protocols::l4::socket::SocketAddr::Inet(addr) => Some(addr.ip()),
        _ => None,
    })
}

fn header_lookup<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

pub(crate) fn pick_emit_codec(c: &Option<AnyCodec>) -> AnyCodec {
    match c {
        Some(AnyCodec::OpenAi(_)) => AnyCodec::OpenAi(gw_codec::openai::OpenAi),
        // OpenAI Responses re-emits through the same vendor family; full
        // shape-preserving emit lands in T6 (S-codec proxy hookup). For now
        // we collapse to OpenAi so the error-emission path keeps producing
        // a valid wire body.
        Some(AnyCodec::OpenAiResponses(_)) => AnyCodec::OpenAi(gw_codec::openai::OpenAi),
        Some(AnyCodec::Anthropic(_)) => AnyCodec::Anthropic(gw_codec::anthropic::Anthropic),
        Some(AnyCodec::Gemini(_)) => AnyCodec::Gemini(gw_codec::gemini::Gemini),
        None => AnyCodec::OpenAi(gw_codec::openai::OpenAi),
    }
}

/// Rebind an `ErrorIR<'static>` (typically from a gw-auth/gw-store helper)
/// to an `ErrorIR<'a>` carrying the per-request `request_id`. Cheap — all
/// `Cow::Borrowed('static)` fields stay borrowed, only the `request_id`
/// pointer is swapped.
pub(crate) fn with_request_id<'a>(src: &ErrorIR<'static>, rid: &'a str) -> ErrorIR<'a> {
    ErrorIR {
        category: src.category,
        status: src.status,
        upstream_status: src.upstream_status,
        code: src.code.clone(),
        message: src.message.clone(),
        retryable: src.retryable,
        upstream_raw: src.upstream_raw,
        request_id: rid,
        retry_after_secs: src.retry_after_secs,
    }
}
