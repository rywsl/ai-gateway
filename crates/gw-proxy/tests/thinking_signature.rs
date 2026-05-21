use gw_core::error::{ClassifierOutcome, ErrorClass, Retryability};
use gw_proxy::error::handler::{ErrorListener, HandlerCtx};
use gw_proxy::error::listeners::thinking::{FlowState, ThinkingSignatureListener};
use std::sync::Arc;

#[test]
fn pre_first_chunk_marks_failover() {
    let state = Arc::new(FlowState::new());
    state.mark_pre_first_chunk();
    let l = ThinkingSignatureListener::new(state.clone());
    let oc = ClassifierOutcome {
        class: ErrorClass::ThinkingSignatureMismatch,
        retryable: Retryability::No,
        ..Default::default()
    };
    l.on_outcome(&HandlerCtx::stub(), &oc);
    assert!(state.should_failover());
}

#[test]
fn after_first_chunk_does_not_failover() {
    let state = Arc::new(FlowState::new());
    state.mark_post_first_chunk();
    let l = ThinkingSignatureListener::new(state.clone());
    let oc = ClassifierOutcome {
        class: ErrorClass::ThinkingSignatureMismatch,
        retryable: Retryability::No,
        ..Default::default()
    };
    l.on_outcome(&HandlerCtx::stub(), &oc);
    assert!(!state.should_failover());
}

#[test]
fn non_thinking_class_ignored() {
    let state = Arc::new(FlowState::new());
    state.mark_pre_first_chunk();
    let l = ThinkingSignatureListener::new(state.clone());
    let oc = ClassifierOutcome {
        class: ErrorClass::UpstreamTransient,
        retryable: Retryability::SameKey,
        ..Default::default()
    };
    l.on_outcome(&HandlerCtx::stub(), &oc);
    assert!(!state.should_failover());
}

#[test]
fn flow_state_default_is_pre_first() {
    let state = FlowState::new();
    assert!(!state.should_failover());
}
