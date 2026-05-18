//! Deduction state arithmetic (pure unit).

use gw_store::deduction::DeductionState;
use std::sync::atomic::AtomicI64;

#[test]
fn real_balance_is_base_minus_deducted_minus_frozen() {
    let s = DeductionState {
        base_micro: AtomicI64::new(1000),
        deducted_micro: AtomicI64::new(200),
        frozen_micro: AtomicI64::new(100),
    };
    assert_eq!(s.real(), 700);
}

#[test]
fn real_balance_can_go_negative() {
    // Deducted + frozen exceeds base — should produce a negative real balance
    // so the deduction pipeline can short-circuit / open the circuit.
    let s = DeductionState {
        base_micro: AtomicI64::new(100),
        deducted_micro: AtomicI64::new(200),
        frozen_micro: AtomicI64::new(50),
    };
    assert_eq!(s.real(), -150);
}
