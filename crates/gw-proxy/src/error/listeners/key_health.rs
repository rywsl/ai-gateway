//! KeyHealth listener. Wave-3 form is a counter-only no-op; T5-A wires the
//! real KeyHealthStore handle into HandlerCtx and converts outcome.cooldown
//! into apply_cooldown calls.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;

#[derive(Default)]
pub struct KeyHealthListener;

impl ErrorListener for KeyHealthListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        if oc.cooldown.is_some() {
            metrics::counter!("gw_classifier_listener_observed_total",
                "listener" => "key_health")
            .increment(1);
        }
    }
}
