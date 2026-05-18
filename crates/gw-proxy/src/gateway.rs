//! `LlmGateway` — the `ProxyHttp` implementation that wires every Phase 1-3
//! crate together.
//!
//! Task 4.1 ships only the skeleton:
//! - `new_ctx` that generates a 16-byte random trace/request ID.
//! - A stub `upstream_peer` that resolves the picked vendor's first base URL
//!   (or falls back to OpenAI) and constructs an `HttpPeer` with default
//!   timeouts.
//!
//! Task 4.2 inserts the full filter chain (early/request/upstream/response/
//! logging) into this impl.

use crate::ctx::ReqCtx;
use crate::timeouts::Timeouts;
use async_trait::async_trait;
use gw_auth::ratelimit::RateLimitGuard;
use gw_core::ids::{RequestId, TraceId};
use gw_ledger::Ledger;
use gw_routing::{BreakerStore, RetryConfig};
use gw_store::EntityStore;
use pingora::upstreams::peer::HttpPeer;
use pingora::Result;
use pingora_proxy::{ProxyHttp, Session};
use rand::RngCore;
use std::sync::Arc;

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

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        ctx.phase_start("upstream_peer");
        // Task 4.2 will replace this stub with the full route → pick → render
        // chain. For 4.1 we resolve from `ctx.picked_vendor` (if Task 4.2 wired
        // it) or fall back to OpenAI so the skeleton has a concrete peer for
        // wiring tests.
        let host = ctx
            .picked_vendor
            .as_ref()
            .and_then(|v| v.value.base_urls.first())
            .and_then(|u| u.host_str())
            .map(str::to_owned)
            .unwrap_or_else(|| "api.openai.com".to_string());
        let mut peer = HttpPeer::new(format!("{host}:443"), true, host);
        Timeouts::default_for_nonstream().apply_to_peer(&mut peer, ctx.is_stream);
        ctx.phase_end();
        Ok(Box::new(peer))
    }
}

/// 16-byte random ID encoded as lowercase hex (32 chars).
fn trace_id_hex() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
