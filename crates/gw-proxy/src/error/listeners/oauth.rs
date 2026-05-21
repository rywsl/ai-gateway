//! OAuth listener stub. T5-A will mark refresh-failed / blocked via K-C.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;

#[derive(Default)]
pub struct OAuthListener;

impl ErrorListener for OAuthListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        if oc.oauth_signal.is_some() {
            metrics::counter!("gw_classifier_listener_observed_total",
                "listener" => "oauth")
            .increment(1);
        }
    }
}
