//! `NoopAdapter` — the default `UpstreamAdapter` impl.
//!
//! Equivalent to the legacy hard-coded x-api-key injection currently in
//! `gw-proxy::filters::upstream`. Phase 3 wires the registry through the
//! proxy + deletes the hard-coded path.

use crate::ctx::AdapterCtx;
use crate::error::AdapterError;
use crate::trait_def::UpstreamAdapter;
use async_trait::async_trait;
use secrecy::ExposeSecret;

pub struct NoopAdapter;

#[async_trait]
impl UpstreamAdapter for NoopAdapter {
    fn name(&self) -> &'static str {
        "noop"
    }

    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let key = ctx.vendor_key.api_key.expose_secret().to_owned();
        ctx.headers_mut.remove("authorization");
        ctx.headers_mut.set("x-api-key", &key);
        Ok(())
    }

    fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        Ok(())
    }
}
