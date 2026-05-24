use gw_proxy::billing_lifecycle::{can_transition, is_terminal, transition_to, VendorExposure};
use gw_proxy::ctx::ReqCtx;

#[test]
fn vendor_exposure_default_is_idle() {
    assert!(matches!(VendorExposure::default(), VendorExposure::Idle));
}

#[test]
fn valid_transitions_pass() {
    use VendorExposure::*;
    for (from, to) in [
        (Idle, Sent),
        (Sent, Streaming),
        (Sent, Terminated),
        (Streaming, Settled),
        (Streaming, Terminated),
    ] {
        assert!(
            can_transition(from, to),
            "{:?} -> {:?} should be allowed",
            from,
            to
        );
    }
}

#[test]
fn invalid_transitions_rejected() {
    use VendorExposure::*;
    for (from, to) in [
        (Idle, Streaming),
        (Idle, Settled),
        (Sent, Settled),
        (Settled, Streaming),
        (Terminated, Settled),
        (Settled, Terminated),
    ] {
        assert!(
            !can_transition(from, to),
            "{:?} -> {:?} should be rejected",
            from,
            to
        );
    }
}

#[test]
fn terminal_states_identified() {
    use VendorExposure::*;
    assert!(is_terminal(Settled));
    assert!(is_terminal(Terminated));
    assert!(!is_terminal(Idle));
    assert!(!is_terminal(Sent));
    assert!(!is_terminal(Streaming));
}

#[test]
fn transition_to_records_sent_ms() {
    let mut ctx = ReqCtx::default();
    assert!(ctx.timing.sent_ms.is_none());
    transition_to(&mut ctx, VendorExposure::Sent);
    assert!(matches!(ctx.vendor_exposure, VendorExposure::Sent));
    assert!(ctx.timing.sent_ms.is_some());
}

#[test]
fn transition_to_records_settled_ms_for_terminal() {
    let mut ctx = ReqCtx::default();
    transition_to(&mut ctx, VendorExposure::Sent);
    transition_to(&mut ctx, VendorExposure::Streaming);
    assert!(ctx.timing.settled_ms.is_none());
    transition_to(&mut ctx, VendorExposure::Settled);
    assert!(ctx.timing.settled_ms.is_some());
}

#[test]
fn transition_to_records_settled_ms_for_terminated() {
    let mut ctx = ReqCtx::default();
    transition_to(&mut ctx, VendorExposure::Sent);
    transition_to(&mut ctx, VendorExposure::Terminated);
    assert!(ctx.timing.settled_ms.is_some());
}

#[test]
fn transition_to_does_not_overwrite_existing_timestamps() {
    let mut ctx = ReqCtx::default();
    transition_to(&mut ctx, VendorExposure::Sent);
    let original_sent_ms = ctx.timing.sent_ms;
    // Re-entering Sent state should NOT change the timestamp (idempotent)
    // Note: the state machine forbids Sent -> Sent, so we can't directly test
    // this -- but transition_to should not panic on same-state re-entry.
    // (We rely on the debug_assert allowing same-state for idempotency.)
    transition_to(&mut ctx, VendorExposure::Sent);
    assert_eq!(ctx.timing.sent_ms, original_sent_ms);
}
