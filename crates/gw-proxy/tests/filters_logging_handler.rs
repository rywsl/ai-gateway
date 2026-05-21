//! Smoke: 6-listener registration shape matches T3-A's expected fanout order.
//! Real filter integration (logging.rs calling fanout) is tested separately
//! via the in-process listeners_wire test from T5-A.

use gw_proxy::error::handler::{ErrorHandler, HandlerCtx};
use gw_proxy::error::listeners::{
    cache_invalidate::CacheInvalidateListener, key_health::KeyHealthListener,
    ledger::LedgerListener, metrics::MetricsListener, oauth::OAuthListener,
    rate_limit::RateLimitFeedbackListener,
};
use std::sync::Arc;

#[test]
#[allow(clippy::default_constructed_unit_structs)]
fn six_listeners_default_construct_and_fanout_no_panic() {
    let h = ErrorHandler::new(vec![
        Arc::new(KeyHealthListener::default()) as _,
        Arc::new(RateLimitFeedbackListener::default()) as _,
        Arc::new(OAuthListener::default()) as _,
        Arc::new(CacheInvalidateListener::default()) as _,
        Arc::new(MetricsListener::default()) as _,
        Arc::new(LedgerListener::default()) as _,
    ]);
    let ctx = HandlerCtx::stub();
    let oc = gw_core::error::ClassifierOutcome::default();
    h.fanout(&ctx, &oc);
}
