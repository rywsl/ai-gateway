use gw_core::error::{
    AccountBlockReason, ClassifierOutcome, CooldownDirective, ErrorClass, OAuthSignal,
    OAuthSignalKind, RateLimitHint, Retryability,
};
use gw_core::ratelimit::RateLimitKind;
use gw_proxy::error::handler::{ErrorHandler, ErrorListener, HandlerCtx};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct CountingListener {
    name: &'static str,
    calls: AtomicUsize,
    order_log: Arc<Mutex<Vec<&'static str>>>,
}

impl ErrorListener for CountingListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, _oc: &ClassifierOutcome) {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.order_log.lock().push(self.name);
    }
}

fn outcome() -> ClassifierOutcome {
    ClassifierOutcome {
        class: ErrorClass::AccountBlocked(AccountBlockReason::Status401Unauthorized),
        retryable: Retryability::CrossKey,
        cooldown: Some(CooldownDirective {
            reason_code: 2,
            ttl_secs: 3600,
            reset_consecutive: false,
        }),
        rate_limit_hint: Some(RateLimitHint {
            kind: RateLimitKind::Unknown,
            retry_after_secs: None,
        }),
        oauth_signal: Some(OAuthSignal { kind: OAuthSignalKind::AccountBlocked }),
        passthrough_body: None,
    }
}

#[test]
fn fanout_invokes_listeners_in_registered_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let make = |name: &'static str| -> Arc<dyn ErrorListener> {
        Arc::new(CountingListener {
            name,
            calls: AtomicUsize::new(0),
            order_log: log.clone(),
        })
    };
    let h = ErrorHandler::new(vec![
        make("key_health"),
        make("rate_limit"),
        make("oauth"),
        make("cache"),
        make("metrics"),
        make("ledger"),
    ]);
    let ctx = HandlerCtx::stub();
    let oc = outcome();
    h.fanout(&ctx, &oc);
    assert_eq!(
        *log.lock(),
        vec!["key_health", "rate_limit", "oauth", "cache", "metrics", "ledger"]
    );
}

#[test]
fn empty_handler_is_noop() {
    let h = ErrorHandler::new(vec![]);
    let ctx = HandlerCtx::stub();
    let oc = outcome();
    h.fanout(&ctx, &oc); // must not panic
}

#[test]
#[allow(clippy::default_constructed_unit_structs)]
fn each_default_listener_compiles_and_runs() {
    use gw_proxy::error::listeners::{
        cache_invalidate::CacheInvalidateListener, key_health::KeyHealthListener,
        ledger::LedgerListener, metrics::MetricsListener, oauth::OAuthListener,
        rate_limit::RateLimitFeedbackListener,
    };
    let h = ErrorHandler::new(vec![
        Arc::new(KeyHealthListener::default()) as _,
        Arc::new(RateLimitFeedbackListener::default()) as _,
        Arc::new(OAuthListener::default()) as _,
        Arc::new(CacheInvalidateListener::default()) as _,
        Arc::new(MetricsListener::default()) as _,
        Arc::new(LedgerListener::default()) as _,
    ]);
    let ctx = HandlerCtx::stub();
    let oc = outcome();
    h.fanout(&ctx, &oc);
}
