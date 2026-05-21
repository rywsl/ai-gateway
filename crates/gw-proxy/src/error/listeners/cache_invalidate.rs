//! CacheInvalidate listener stub. T5-A wires EntityStore handle into ctx
//! and invalidates ApiKey on AuthDenied / AccountBlocked.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::{ClassifierOutcome, ErrorClass};

#[derive(Default)]
pub struct CacheInvalidateListener;

impl ErrorListener for CacheInvalidateListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        if matches!(
            oc.class,
            ErrorClass::AuthDenied | ErrorClass::AccountBlocked(_)
        ) {
            metrics::counter!("gw_classifier_listener_observed_total",
                "listener" => "cache")
            .increment(1);
        }
    }
}
