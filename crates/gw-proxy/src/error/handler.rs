//! ErrorHandler — fans out a ClassifierOutcome to a fixed-order list of
//! listeners. Real listener side-effects are wired in T5-A; this task only
//! lands the trait + registry shape.

use gw_core::error::ClassifierOutcome;
use std::sync::Arc;

/// Listener trait. Each implementor is called serially (no `async`, no I/O —
/// listeners that need I/O must `tokio::spawn` themselves).
pub trait ErrorListener: Send + Sync {
    fn on_outcome(&self, ctx: &HandlerCtx<'_>, outcome: &ClassifierOutcome);
}

/// Fixed-order list of listeners. Registration order is the canonical fanout
/// order; reordering is a behavior change and must be reviewed.
pub struct ErrorHandler {
    listeners: Vec<Arc<dyn ErrorListener>>,
}

impl ErrorHandler {
    pub fn new(listeners: Vec<Arc<dyn ErrorListener>>) -> Self {
        Self { listeners }
    }

    pub fn fanout(&self, ctx: &HandlerCtx<'_>, outcome: &ClassifierOutcome) {
        for l in &self.listeners {
            l.on_outcome(ctx, outcome);
        }
    }
}

/// Per-request context passed to listeners.
///
/// Real fields (key_health store handle, entity_store, resilience) are wired
/// in T5-A. Stub form supports unit-testing fanout shape.
pub struct HandlerCtx<'a> {
    pub request_id: &'a str,
    pub api_key_id: Option<&'a str>,
    pub vendor_key_id: Option<&'a str>,
    pub api_format: Option<gw_core::protocol::ProtocolKind>,
}

impl HandlerCtx<'_> {
    pub fn stub() -> Self {
        Self {
            request_id: "",
            api_key_id: None,
            vendor_key_id: None,
            api_format: None,
        }
    }
}
