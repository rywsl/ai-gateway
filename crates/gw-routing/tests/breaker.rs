//! Unit tests for the breaker state machine.

use gw_routing::breaker::*;
use std::sync::atomic::Ordering;

#[test]
fn closed_until_failure_rate_exceeds() {
    let b = BreakerState::new();
    // 15 failures < 20 samples → still closed.
    for _ in 0..15 {
        b.record(false);
    }
    assert!(b.is_available());

    // +10 failures → 25 samples, 100% fail → open.
    for _ in 0..10 {
        b.record(false);
    }
    assert!(!b.is_available());
}

#[test]
fn open_to_half_open_after_open_until_elapses() {
    let b = BreakerState::new();
    for _ in 0..25 {
        b.record(false);
    }
    assert!(!b.is_available());

    // Force the open deadline to elapse immediately.
    b.open_until_ms.store(0, Ordering::Release);
    assert!(b.is_available());
    // CAS-flipped from open (1) to half-open (2).
    assert_eq!(b.circuit.load(Ordering::Acquire), 2);
    // A subsequent caller is rejected: the probe is exclusive.
    assert!(!b.is_available());
}

#[test]
fn half_open_closes_on_success() {
    let b = BreakerState::new();
    // Put the breaker in half-open directly. We avoid driving it through
    // record(false) because the trip branch (≥20 samples, >50% fail) takes
    // precedence over the close-on-success branch and would re-open the
    // circuit on the next record() call.
    b.circuit.store(2, Ordering::Release);
    b.record(true);
    assert_eq!(b.circuit.load(Ordering::Acquire), 0);
}
