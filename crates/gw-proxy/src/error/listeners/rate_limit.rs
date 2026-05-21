//! RateLimitFeedback listener. Routes `rate_limit_hint` into observability for
//! now; the RL-Adaptive `observe(...)` wire lands when RL-Adaptive ships in
//! its own spec. T5-A only adds the kind label so the metric is queryable
//! per `RateLimitKind` variant.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;

#[derive(Default)]
pub struct RateLimitFeedbackListener;

impl ErrorListener for RateLimitFeedbackListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        let Some(hint) = &oc.rate_limit_hint else {
            return;
        };
        let kind = format!("{:?}", hint.kind);
        metrics::counter!(
            "gw_classifier_listener_observed_total",
            "listener" => "rate_limit",
            "kind" => kind,
        )
        .increment(1);
    }
}
