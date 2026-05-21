//! `LlmGateway` — the `ProxyHttp` implementation that wires every Phase 1-3
//! crate together. Each method delegates to a `filters::*` module.
//!
//! Construction is via [`LlmGateway::new`] which takes pre-built `Arc`s for
//! every long-lived collaborator. The `gw-gateway` binary owns these and the
//! gateway itself is `Sync`-clean so Pingora can hand it to the service
//! framework.

use crate::ctx::ReqCtx;
use crate::filters;
use async_trait::async_trait;
use bytes::Bytes;
use gw_auth::ratelimit::RateLimitGuard;
use gw_core::ids::{RequestId, TraceId};
use gw_ledger::Ledger;
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_store::{CounterStore, EntityStore};
use pingora::http::{RequestHeader, ResponseHeader};
use pingora::upstreams::peer::HttpPeer;
use pingora::Result;
use pingora_proxy::{ProxyHttp, Session};
use rand::RngCore;
use std::sync::Arc;
use std::time::Duration;

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
        ReqCtx::new(trace, req)
    }

    async fn early_request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        filters::early::run(session, ctx, &self.store, &self.ratelimit, self.trust_xff).await
    }

    async fn request_filter(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<bool> {
        filters::request::run(
            session,
            ctx,
            &self.store,
            &self.ratelimit,
            &self.counters,
            self.global_rpm,
            self.trust_xff,
            self.jwt_validator.as_ref(),
        )
        .await
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        ctx.phase_start("upstream_peer");
        let peer = filters::upstream::pick_peer(session, ctx, &self.store, &self.health, &self.in_flight).await?;
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

    async fn logging(
        &self,
        session: &mut Session,
        err: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        filters::logging::run(session, err, ctx, &self.health, &self.ledger, &self.counters)
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
    use pingora::ErrorType;
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
    use super::is_gw_short_circuit;
    use pingora::{Error, ErrorType};

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
