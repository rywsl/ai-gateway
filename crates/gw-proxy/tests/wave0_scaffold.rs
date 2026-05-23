//! Wave 0 compile-time assertions for the known-gaps scaffold.
//!
//! These tests do not exercise behaviour — if the binary builds and these
//! assertions hold, the struct shape and error-code constants are wired in.
//! Subsequent waves (T1-A … T4-A) layer real logic on top of these slots.

use gw_proxy::ctx::ReqCtx;

#[test]
fn reqctx_has_wave0_fields() {
    let ctx = ReqCtx::default();
    let _: &Vec<gw_core::ids::KeyId> = &ctx.retry_blacklist;
    let _: &Option<bytes::BytesMut> = &ctx.pending_client_body;
    let _: u32 = ctx.retry_count;
}

#[test]
fn error_codes_are_defined() {
    use gw_core::error::codes::*;
    assert_eq!(GW_AUTH_KEY_EXPIRED, "GW_AUTH_KEY_EXPIRED");
    assert_eq!(GW_AUTH_KEY_IP_NOT_ALLOWED, "GW_AUTH_KEY_IP_NOT_ALLOWED");
    assert_eq!(GW_AUTH_JWT_INVALID_ISS, "GW_AUTH_JWT_INVALID_ISS");
    assert_eq!(GW_AUTH_JWT_INVALID_AUD, "GW_AUTH_JWT_INVALID_AUD");
    assert_eq!(GW_AUTH_JWT_NOT_YET_VALID, "GW_AUTH_JWT_NOT_YET_VALID");
    assert_eq!(GW_AUTH_SIGNATURE_MISSING, "GW_AUTH_SIGNATURE_MISSING");
    assert_eq!(
        GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT,
        "GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT"
    );
    assert_eq!(
        GW_QUOTA_DAILY_TOKENS_EXCEEDED,
        "GW_QUOTA_DAILY_TOKENS_EXCEEDED"
    );
    assert_eq!(
        GW_ROUTING_NO_CAPABILITY_MATCH,
        "GW_ROUTING_NO_CAPABILITY_MATCH"
    );
    assert_eq!(GW_UPSTREAM_CONNECT_FAILED, "GW_UPSTREAM_CONNECT_FAILED");
    assert_eq!(GW_UPSTREAM_TLS_ERROR, "GW_UPSTREAM_TLS_ERROR");
}

#[test]
fn entity_store_has_auth_config_cache() {
    // Pure compile check — exercise the cache via its public surface (Moka
    // exposes `entry_count`) and a typed `get` call. If `EntityStore::auth_config`
    // is the wrong key/value shape this won't compile.
    async fn _t(
        s: &gw_store::entity_store::EntityStore,
    ) -> Option<std::sync::Arc<gw_core::versioned::Versioned<gw_core::entity::AuthConfig>>> {
        let _: u64 = s.auth_config.entry_count();
        s.auth_config.get("global").await
    }
}
