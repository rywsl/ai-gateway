//! ErrorHandler — fans out a ClassifierOutcome to a fixed-order list of
//! listeners. T5-A wires real dependency handles (KeyHealthStore, EntityStore,
//! ResilienceState) into HandlerCtx so listener side-effects can land. The
//! handles are all `Option`: tests use `HandlerCtx::stub()` to construct a
//! no-op variant, and listeners short-circuit when their required handle is
//! absent.

use crate::resilience::ResilienceState;
use gw_core::error::ClassifierOutcome;
use gw_health::KeyHealthStore;
use gw_store::EntityStore;
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
/// `request_id`, `api_key_id`, `vendor_key_id`, `api_format` are populated by
/// the proxy logging filter from the request CTX. `key_health`, `entity_store`,
/// `resilience` are long-lived handles wired in from gw-gateway bootstrap;
/// listeners that need them must check for `Some(_)` and degrade gracefully
/// when the handle is absent (e.g. unit tests).
pub struct HandlerCtx<'a> {
    pub request_id: &'a str,
    pub api_key_id: Option<&'a str>,
    pub vendor_key_id: Option<&'a str>,
    pub api_format: Option<gw_core::protocol::ProtocolKind>,
    pub key_health: Option<&'a KeyHealthStore>,
    pub entity_store: Option<&'a Arc<EntityStore>>,
    pub resilience: Option<&'a ResilienceState>,
}

impl HandlerCtx<'_> {
    pub fn stub() -> Self {
        Self {
            request_id: "",
            api_key_id: None,
            vendor_key_id: None,
            api_format: None,
            key_health: None,
            entity_store: None,
            resilience: None,
        }
    }
}
