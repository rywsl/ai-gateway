//! `UpstreamAdapter` v2 - 4 hooks + `transport_profile` declaration.
//!
//! Spec: A-T0 ADR-AT0-1 and F1. Each vendor adapter implements this trait
//! for proxy outbound request rewriting. The task worker does not call
//! adapters; task submission must pre-render wire bytes before enqueue and
//! the worker sends those bytes verbatim.

use crate::ctx::AdapterCtx;
use crate::error::AdapterError;
use async_trait::async_trait;
use gw_codec::wire::WireAnyRequest;

#[async_trait]
pub trait UpstreamAdapter: Send + Sync + 'static {
    /// Adapter identifier (e.g. `"claude_code"`, `"vertex_ai"`).
    fn name(&self) -> &'static str;

    /// Rewrite auth headers (OAuth bearer, SA JWT, x-api-key, etc.).
    /// Async because OAuth refresh may be needed (K-C).
    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError>;

    /// Rewrite URL / host / TLS profile. Synchronous — availability probes
    /// are cached.
    fn rewrite_transport(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError>;

    /// Modify the outgoing request wire via WireAny semantic dispatch.
    /// Default: no-op.
    fn patch_request(
        &self,
        _ctx: &AdapterCtx<'_>,
        _wire: &mut WireAnyRequest<'_>,
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Whether production should pay the parse/patch/re-render cost for this
    /// adapter. Default adapters keep byte-identical passthrough bodies.
    fn request_patch_enabled(&self) -> bool {
        false
    }

    /// Modify the incoming response wire. Default: no-op.
    fn patch_response(&self, _ctx: &AdapterCtx<'_>) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Browser-fingerprint declaration. `None` means standard Pingora
    /// transport; `Some(profile)` lets the proxy select its existing
    /// impersonate-capable egress path. The adapter does not implement TLS
    /// fingerprinting itself.
    fn transport_profile(&self) -> Option<TransportProfile> {
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportProfile {
    Standard,
    ChromeImpersonate { version: ChromeVersion },
    FirefoxImpersonate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChromeVersion {
    V110,
    V116,
    V119,
    V120,
    V123,
    V124,
    V131,
}

#[cfg(test)]
mod trait_surface_tests {
    use super::*;

    struct DummyAdapter;

    #[async_trait]
    impl UpstreamAdapter for DummyAdapter {
        fn name(&self) -> &'static str {
            "dummy"
        }
        async fn rewrite_auth(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
            Ok(())
        }
        fn rewrite_transport(&self, _ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
            Ok(())
        }
    }

    #[test]
    fn dummy_compiles_and_defaults_patch_hooks_to_noop() {
        let d = DummyAdapter;
        assert_eq!(d.name(), "dummy");
        assert!(d.transport_profile().is_none());
    }
}
