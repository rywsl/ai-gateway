//! Metrics listener: records the ErrorClass label per outcome.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;

#[derive(Default)]
pub struct MetricsListener;

impl ErrorListener for MetricsListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        let class = format!("{:?}", oc.class);
        metrics::counter!("gw_classifier_outcome_total", "class" => class).increment(1);
    }
}
