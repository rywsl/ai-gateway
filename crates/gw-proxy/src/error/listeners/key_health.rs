//! KeyHealth listener. Wave-3 form was a counter-only no-op; T5-A wires the
//! real `KeyHealthStore` handle into `HandlerCtx`. The actual
//! `apply_cooldown` invocation site (translating
//! `gw_core::error::CooldownDirective::reason_code` →
//! `gw_health::CooldownReason`) lands when filters::logging starts populating
//! `vendor_key_id` + `api_format` on the ctx in T6-B; for now the wire only
//! observes that the dependencies are reachable and emits telemetry.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;

#[derive(Default)]
pub struct KeyHealthListener;

impl ErrorListener for KeyHealthListener {
    fn on_outcome(&self, ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        if oc.cooldown.is_none() {
            return;
        }
        let wired = ctx.vendor_key_id.is_some()
            && ctx.api_format.is_some()
            && ctx.key_health.is_some();
        let kind = if wired { "wired" } else { "unwired" };
        metrics::counter!(
            "gw_classifier_listener_observed_total",
            "listener" => "key_health",
            "kind" => kind,
        )
        .increment(1);
    }
}
