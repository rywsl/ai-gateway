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
}
