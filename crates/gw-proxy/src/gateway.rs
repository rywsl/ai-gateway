//! `LlmGateway` — the `ProxyHttp` implementation that wires every Phase 1-3
//! crate together. Each method delegates to a `filters::*` module.
//!
//! Construction is via [`LlmGateway::new`] which takes pre-built `Arc`s for
//! every long-lived collaborator. The `gw-gateway` binary owns these and the
//! gateway itself is `Sync`-clean so Pingora can hand it to the service
//! framework.

use crate::ctx::ReqCtx;
use crate::filters;
use crate::resilience::ResilienceState;
use async_trait::async_trait;
use bytes::Bytes;
use gw_auth::jwt_blacklist::RedisJwtBlacklist;
use gw_auth::ratelimit::RateLimitGuard;
use gw_core::error::codes;
use gw_core::ids::{RequestId, TraceId};
use gw_health::KeyHealthStore;
use gw_ledger::Ledger;
use gw_routing::in_flight::InFlightStore;
use gw_store::{CounterStore, EntityStore};
use pingora::http::{RequestHeader, ResponseHeader};
use pingora::upstreams::peer::HttpPeer;
use pingora::{Error, ErrorType, Result};
use pingora_proxy::{ProxyHttp, Session};
use rand::RngCore;
use std::sync::Arc;
use std::time::Duration;

/// Maximum cross-key connect retries per request (G10). At
/// `MAX_CONNECT_RETRIES = 2` a request can try at most three keys before
/// giving up: the initial pick plus two retries. Kept conservative because
/// the body is buffered in memory and each retry walks the strategy stack
/// again.
const MAX_CONNECT_RETRIES: u32 = 2;

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

pub struct LlmGateway {
    pub store: Arc<EntityStore>,
    pub health: Arc<KeyHealthStore>,
    pub in_flight: Arc<InFlightStore>,
    pub ratelimit: Arc<RateLimitGuard>,
    pub ledger: Arc<Ledger>,
    pub counters: Arc<CounterStore>,
    pub global_rpm: u32,
    /// When `true`, parse the client IP from the `X-Forwarded-For` header
    /// (using the first / left-most entry, which represents the original
    /// client per RFC 7239 convention). When `false`, use the L4 socket
    /// peer address only. Default is `false` so a misconfigured deployment
    /// does not silently bypass IP blocklisting via a forged header.
    pub trust_xff: bool,
    /// JWT claim validator (iss/aud/nbf, T1-E). `None` until T1-E builds it
    /// from `AuthConfig.global`; auth falls back to signature-only checks.
    pub jwt_validator: Option<Arc<gw_auth::jwt::JwtValidator>>,
    /// Shared selection-strategy instances (T0-B). One `StrategyAny` per kind,
    /// dispatched via `StrategySet::pick_for`.
    pub strategies: Arc<gw_routing::strategy::set::StrategySet>,
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
    /// behaviour byte-identical to pre-tunnel builds. Built under
    /// `feature = "tunnel"` by `gw-gateway::bootstrap` only when
    /// `cfg.tunnel.enabled = true`.
    #[cfg(feature = "tunnel")]
    pub tunnel: Option<Arc<crate::tunnel_stack::TunnelStack>>,
    /// Detailed-log archive (T10). Sender handle for per-surface wire-fidelity
    /// capture. `try_send` is fire-and-forget; a full channel drops + metrics.
    pub(crate) detailed_archive: std::sync::Arc<gw_ledger::detailed_archive::DetailedArchive>,
}

impl LlmGateway {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<EntityStore>,
        health: Arc<KeyHealthStore>,
        in_flight: Arc<InFlightStore>,
        ratelimit: Arc<RateLimitGuard>,
        ledger: Arc<Ledger>,
        counters: Arc<CounterStore>,
        global_rpm: u32,
        trust_xff: bool,
        jwt_validator: Option<Arc<gw_auth::jwt::JwtValidator>>,
        strategies: Arc<gw_routing::strategy::set::StrategySet>,
        capability_resolver: Option<Arc<gw_capability::resolver::CapabilityResolver>>,
        mgmt_blacklist: Arc<RedisJwtBlacklist>,
        resilience: Arc<ResilienceState>,
        #[cfg(feature = "tunnel")] tunnel: Option<Arc<crate::tunnel_stack::TunnelStack>>,
        detailed_archive: std::sync::Arc<gw_ledger::detailed_archive::DetailedArchive>,
    ) -> Self {
        Self {
            store,
            health,
            in_flight,
            ratelimit,
            ledger,
            counters,
            global_rpm,
            trust_xff,
            jwt_validator,
            strategies,
            capability_resolver,
            mgmt_blacklist,
            resilience,
            #[cfg(feature = "tunnel")]
            tunnel,
            detailed_archive,
        }
    }

    /// Accessor for the shared `ResilienceState`. Used by `gw-gateway` /
    /// integration tests that need to observe the same counters the proxy
    /// fanout writes. The field is already `pub` so direct access works,
    /// but this returns the borrow without cloning the `Arc`.
    pub fn resilience(&self) -> &Arc<ResilienceState> {
        &self.resilience
    }
}

#[async_trait]
impl ProxyHttp for LlmGateway {
    type CTX = ReqCtx;

    fn new_ctx(&self) -> Self::CTX {
        let id = trace_id_hex();
        let trace = TraceId::from(id.clone());
        let req = RequestId::from(id);
        ReqCtx::new(trace, req)
    }

    async fn early_request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()> {
        filters::early::run(session, ctx, &self.store, &self.ratelimit, self.trust_xff).await
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        filters::request::run(
            session,
            ctx,
            &self.store,
            &self.ratelimit,
            &self.counters,
            self.global_rpm,
            self.trust_xff,
            self.jwt_validator.as_ref(),
            self.capability_resolver.as_ref(),
            &self.mgmt_blacklist,
        )
        .await
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        ctx.phase_start("upstream_peer");
        let peer = filters::upstream::pick_peer(
            session,
            ctx,
            &self.store,
            &self.health,
            &self.in_flight,
            &self.strategies,
            #[cfg(feature = "tunnel")]
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
        filters::upstream::rewrite_request(session, upstream_request, ctx).await
    }

    /// Replace the upstream-bound body with the codec-rewritten payload (if any).
    ///
    /// `upstream_request_filter` produced the final outbound bytes and stashed
    /// them on `ctx.cached_body`. Without this hook, pingora would forward the
    /// ORIGINAL downstream body to the upstream, so cross-protocol re-renders
    /// would never reach the wire.
    ///
    /// For same-protocol passthrough the rewritten body equals the original, so
    /// the conditional replacement is still correct. We only swap on the final
    /// chunk so chunked-encoding clients still stream cleanly through pingora.
    ///
    /// `cached_body` is cloned rather than taken because the `logging` filter
    /// reads it later to build the optional request-body preview when debug
    /// mode is on. `Bytes` is a cheap refcount, so the clone is effectively
    /// free.
    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        if end_of_stream {
            if let Some(rewritten) = ctx.cached_body.as_ref() {
                *body = Some(rewritten.clone());
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
        Ok(())
    }

    fn response_body_filter(
        &self,
        session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<Duration>> {
        filters::response::run(session, body, end_of_stream, ctx)
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
    /// v1.5 scope: this hook handles **connect-only** retry. Response-side
    /// (5xx / 429) retry remains deferred to the routing-r-e `FailoverExecutor v2`.
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
                e
            }
            ConnectErrorKind::Connect => {
                if ctx.retry_count < MAX_CONNECT_RETRIES {
                    if let Some(k) = ctx.picked_vendor_key.as_ref().map(|p| p.key_id.clone()) {
                        ctx.retry_blacklist.push(k);
                    }
                    ctx.retry_count += 1;
                    // Drop the in-flight guard for the failed key so the next
                    // pick is not counted against the previous key's quota.
                    ctx.inflight_guard = None;
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
                    e
                }
            }
        }
    }

    async fn logging(
        &self,
        session: &mut Session,
        err: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        filters::logging::run(
            session,
            err,
            ctx,
            &self.health,
            &self.ledger,
            &self.counters,
        )
        .await;
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
    use super::{classify_connect_error, is_gw_short_circuit, ConnectErrorKind};
    use pingora::{Error, ErrorType};

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
}
