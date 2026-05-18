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
use gw_routing::{BreakerStore, RetryConfig};
use gw_store::EntityStore;
use pingora::http::{RequestHeader, ResponseHeader};
use pingora::upstreams::peer::HttpPeer;
use pingora::Result;
use pingora_proxy::{ProxyHttp, Session};
use rand::RngCore;
use std::sync::Arc;
use std::time::Duration;

pub struct LlmGateway {
    pub store: Arc<EntityStore>,
    pub breakers: Arc<BreakerStore>,
    pub ratelimit: Arc<RateLimitGuard>,
    pub ledger: Arc<Ledger>,
    pub retry_cfg: RetryConfig,
    pub global_rpm: u32,
}

impl LlmGateway {
    pub fn new(
        store: Arc<EntityStore>,
        breakers: Arc<BreakerStore>,
        ratelimit: Arc<RateLimitGuard>,
        ledger: Arc<Ledger>,
        retry_cfg: RetryConfig,
        global_rpm: u32,
    ) -> Self {
        Self {
            store,
            breakers,
            ratelimit,
            ledger,
            retry_cfg,
            global_rpm,
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
        filters::early::run(session, ctx, &self.store).await
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
            self.global_rpm,
        )
        .await
    }

    async fn upstream_peer(
        &self,
        session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        ctx.phase_start("upstream_peer");
        let peer = filters::upstream::pick_peer(session, ctx, &self.store, &self.breakers).await?;
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
        filters::logging::run(session, err, ctx, &self.breakers, &self.ledger).await;
    }
}

/// 16-byte random ID encoded as lowercase hex (32 chars).
fn trace_id_hex() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
