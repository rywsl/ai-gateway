//! Ledger listener stub. T5-A will emit detailed_log entries via the sink
//! introduced in T3-E.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::ClassifierOutcome;

#[derive(Default)]
pub struct LedgerListener;

impl ErrorListener for LedgerListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, _oc: &ClassifierOutcome) {
        // No-op until Wave 5/6 wires detailed log.
    }
}
