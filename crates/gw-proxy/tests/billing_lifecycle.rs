use gw_proxy::billing_lifecycle::{can_transition, is_terminal, VendorExposure};

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
