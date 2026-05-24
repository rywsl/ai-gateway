//! Ledger listener — emits structured error-path log entries and a counter
//! for every classified error outcome.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;

#[derive(Default)]
pub struct LedgerListener;

impl ErrorListener for LedgerListener {
    fn on_outcome(&self, ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        tracing::info!(
            target: "gw_error_ledger",
            request_id = %ctx.request_id,
            api_key_id = ?ctx.api_key_id,
            vendor_key_id = ?ctx.vendor_key_id,
            error_class = ?oc.class,
            retryable = ?oc.retryable,
            has_cooldown = oc.cooldown.is_some(),
            has_rate_limit_hint = oc.rate_limit_hint.is_some(),
            "error outcome recorded"
        );
        metrics::counter!("gw_error_handler_ledger_total").increment(1);
    }
}
