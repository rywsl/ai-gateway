//! ThinkingSignatureListener — gate the failover decision when the upstream
//! response shows a `ThinkingSignatureMismatch`.
//!
//! Flow contract:
//! - 0 = pre-first-chunk: the response head has not been forwarded yet, so a
//!   failover to a different vendor key is safe.
//! - 1 = post-first-chunk: the client already sees response bytes; failing
//!   over here would surface a corrupt stream.
//! - 2 = failover-requested: state set by this listener when (state == 0)
//!   and `ErrorClass::ThinkingSignatureMismatch` arrives. R-F (T6-D follow)
//!   reads via `should_failover()`.

use crate::error::handler::{ErrorListener, HandlerCtx};
use gw_core::error::{ClassifierOutcome, ErrorClass};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

pub struct FlowState {
    st: AtomicU8,
}

impl FlowState {
    pub fn new() -> Self {
        Self {
            st: AtomicU8::new(0),
        }
    }

    pub fn mark_pre_first_chunk(&self) {
        self.st.store(0, Ordering::Release);
    }

    pub fn mark_post_first_chunk(&self) {
        self.st.store(1, Ordering::Release);
    }

    pub fn should_failover(&self) -> bool {
        self.st.load(Ordering::Acquire) == 2
    }

    fn request_failover(&self) {
        let _ = self
            .st
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl Default for FlowState {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ThinkingSignatureListener {
    flow: Arc<FlowState>,
}

impl ThinkingSignatureListener {
    pub fn new(flow: Arc<FlowState>) -> Self {
        Self { flow }
    }
}

impl ErrorListener for ThinkingSignatureListener {
    fn on_outcome(&self, _ctx: &HandlerCtx<'_>, oc: &ClassifierOutcome) {
        if matches!(oc.class, ErrorClass::ThinkingSignatureMismatch) {
            self.flow.request_failover();
        }
    }
}
