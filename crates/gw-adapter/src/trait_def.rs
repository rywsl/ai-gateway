//! `UpstreamAdapter` v2 — 4 hooks + `transport_profile` declaration.
//!
//! Spec: A-T0 §ADR-AT0-1. Each vendor adapter (claude_code / gemini_cli /
//! vertex_ai / antigravity / codex / kiro) implements this trait; the proxy
//! and the task runtime call into it via `AdapterRegistry::for_vendor`.

use crate::ctx::AdapterCtx;
use crate::error::AdapterError;
use async_trait::async_trait;
use gw_codec::wire::{WireAnyRequest, WireAnyResponse};

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

    /// Modify the incoming response wire. Default: no-op.
    fn patch_response(
        &self,
        _ctx: &AdapterCtx<'_>,
        _wire: &mut WireAnyResponse<'_>,
    ) -> Result<(), AdapterError> {
        Ok(())
    }

    /// Browser-fingerprint declaration (A12). `None` = standard reqwest /
    /// pingora; `Some(profile)` = the http-client selector picks an
    /// impersonate-capable backend. Full backend selection lands in a
    /// future spec; in v1 a non-`None` value warns and falls back to
    /// `Standard`.
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
