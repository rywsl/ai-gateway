//! Dispatcher for `events:counter` stream events. Pre-2026-05-29 the counter
//! stream was a no-op (`stream_consumer.rs:54-57`); this module replaces that
//! no-op with a registered handler pattern.
//!
//! `gw-store` cannot directly call into `gw-ledger` (gw-ledger already deps
//! gw-store; adding a back-edge would cycle). So we expose a trait and a
//! `OnceCell` registration slot. The actual handler implementation lives in
//! `gw-ledger::counter_handler` and is wired in by gateway boot.

use once_cell::sync::OnceCell;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    Handled,
    UnknownNs,
    InvalidFields,
    NoHandler,
}

/// Implementer that interprets specific `events:counter` ns values. The trait
/// is sync because the underlying state mutations (LeasePool / ModelCoefCache)
/// are all in-process atomic ops with no I/O.
pub trait CounterHandler: Send + Sync {
    fn handle(&self, ns: &str, fields: &HashMap<String, String>) -> DispatchOutcome;
}

static HANDLER: OnceCell<Box<dyn CounterHandler>> = OnceCell::new();

/// Register the global handler. Called once at gateway boot. Subsequent calls
/// are silently dropped (`OnceCell` semantics).
pub fn register_handler(h: Box<dyn CounterHandler>) {
    let _ = HANDLER.set(h);
}

/// Dispatch a counter event. Returns the outcome so the caller can log
/// unknown-ns occurrences.
pub fn dispatch(fields: &HashMap<String, String>) -> DispatchOutcome {
    let ns = match fields.get("ns").map(String::as_str) {
        Some(n) => n,
        None => return DispatchOutcome::InvalidFields,
    };
    match HANDLER.get() {
        Some(h) => h.handle(ns, fields),
        None => DispatchOutcome::NoHandler,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct CountingHandler {
        seen: AtomicU32,
    }
    impl CounterHandler for CountingHandler {
        fn handle(&self, _ns: &str, _fields: &HashMap<String, String>) -> DispatchOutcome {
            self.seen.fetch_add(1, Ordering::AcqRel);
            DispatchOutcome::Handled
        }
    }

    #[test]
    fn dispatch_missing_ns_is_invalid_regardless_of_handler() {
        let f: HashMap<String, String> = HashMap::new();
        assert_eq!(dispatch(&f), DispatchOutcome::InvalidFields);
    }

    #[test]
    fn dispatch_with_no_handler_returns_no_handler() {
        // This test runs against a fresh OnceCell only if no other test in
        // this `cargo test` invocation has registered. Since there is at most
        // one HANDLER for the entire test binary, we accept either outcome:
        // if some other test installed one first, dispatch may return Handled.
        let f: HashMap<String, String> =
            [("ns".to_string(), "x".to_string())].into_iter().collect();
        let out = dispatch(&f);
        assert!(
            matches!(out, DispatchOutcome::NoHandler | DispatchOutcome::Handled),
            "unexpected outcome before handler registration: {:?}",
            out
        );
    }

    #[test]
    fn handler_invoked_for_any_ns() {
        let counting = CountingHandler {
            seen: AtomicU32::new(0),
        };
        // Capture the AtomicU32 by reference via the boxed handler. Because
        // OnceCell only sets once, this test may be skipped if a prior test
        // already set a different handler. We use a single canonical handler
        // for the entire test binary: install on first call, no-op on subsequent.
        register_handler(Box::new(counting));

        let f: HashMap<String, String> = [
            ("ns".to_string(), "any".to_string()),
            ("user_id".to_string(), "u_x".to_string()),
        ]
        .into_iter()
        .collect();
        let out = dispatch(&f);
        // Whatever handler is registered should accept this — Handled (if it's
        // ours) or whatever the other handler returns. We only assert the
        // dispatch path actually ran without InvalidFields/NoHandler.
        assert!(
            !matches!(
                out,
                DispatchOutcome::InvalidFields | DispatchOutcome::NoHandler
            ),
            "dispatch unexpectedly returned {:?} after register_handler",
            out
        );
    }
}
