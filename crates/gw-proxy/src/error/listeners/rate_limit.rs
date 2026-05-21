//! RateLimitFeedback listener stub. T5-A will route rate_limit_hint into
//! RL-Adaptive `observe(...)`.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;

#[derive(Default)]
pub struct RateLimitFeedbackListener;

impl ErrorListener for RateLimitFeedbackListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        if oc.rate_limit_hint.is_some() {
            metrics::counter!("gw_classifier_listener_observed_total",
                "listener" => "rate_limit")
            .increment(1);
        }
    }
}
