//! T5-A wire test — fanout with the 5 default listeners must not panic when
//! fed a fully-populated outcome, and `HandlerCtx::stub()` must populate the
//! optional real-dependency fields with `None`.

use gw_core::error::{
    AccountBlockReason, ClassifierOutcome, CooldownDirective, ErrorClass, OAuthSignal,
    OAuthSignalKind, RateLimitHint, Retryability,
};
use gw_core::ratelimit::RateLimitKind;
use gw_proxy::error::handler::{ErrorHandler, ErrorListener, HandlerCtx};
use gw_proxy::error::listeners::{
    cache_invalidate::CacheInvalidateListener, key_health::KeyHealthListener,
    metrics::MetricsListener, oauth::OAuthListener, rate_limit::RateLimitFeedbackListener,
};
use std::sync::Arc;

#[test]
#[allow(clippy::default_constructed_unit_structs)]
fn fanout_with_real_ctx_does_not_panic() {
    let h = ErrorHandler::new(vec![
        Arc::new(KeyHealthListener::default()) as Arc<dyn ErrorListener>,
        Arc::new(RateLimitFeedbackListener::default()) as Arc<dyn ErrorListener>,
        Arc::new(OAuthListener::default()) as Arc<dyn ErrorListener>,
        Arc::new(CacheInvalidateListener::default()) as Arc<dyn ErrorListener>,
        Arc::new(MetricsListener::default()) as Arc<dyn ErrorListener>,
    ]);
    let ctx = HandlerCtx::stub();
    let oc = ClassifierOutcome {
        class: ErrorClass::AccountBlocked(AccountBlockReason::Status401Unauthorized),
        retryable: Retryability::CrossKey,
        cooldown: Some(CooldownDirective {
            reason_code: 2,
            ttl_secs: 3600,
            reset_consecutive: false,
        }),
        oauth_signal: Some(OAuthSignal {
            kind: OAuthSignalKind::AccountBlocked,
        }),
        rate_limit_hint: Some(RateLimitHint {
            kind: RateLimitKind::Unknown,
            retry_after_secs: None,
        }),
        passthrough_body: None,
    };
    h.fanout(&ctx, &oc);
}

#[test]
fn ctx_carries_optional_real_fields() {
    let ctx = HandlerCtx::stub();
    assert!(ctx.api_key_id.is_none());
    assert!(ctx.vendor_key_id.is_none());
    assert!(ctx.api_format.is_none());
    assert!(ctx.key_health.is_none());
    assert!(ctx.entity_store.is_none());
    assert!(ctx.resilience.is_none());
    assert!(ctx.affinity.is_none());
}
