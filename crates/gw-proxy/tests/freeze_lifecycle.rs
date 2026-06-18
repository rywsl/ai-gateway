//! RequestFreeze lifecycle integration tests (gw-proxy view).
//!
//! Mirrors gw-ledger's own tests but exercised through the proxy's local
//! re-export so any future refactor of `RequestFreeze` is caught here too.
//!
//! Critical invariant (C-γ §ADR-Cγ-1): release MUST NOT touch
//! deducted_micro. Go is the sole writer of that field via the
//! usage:records -> cost -> events:counter -> Rust loop (Phase 5).

use gw_core::ids::UserId;
use gw_ledger::{FreezeGuard, RequestFreeze};
use gw_store::deduction::DEDUCT;
use std::sync::atomic::Ordering;

fn deducted(uid: &UserId) -> i64 {
    DEDUCT
        .get(uid)
        .map(|e| e.value().deducted_micro.load(Ordering::Acquire))
        .unwrap_or(0)
}

fn frozen(uid: &UserId) -> i64 {
    DEDUCT
        .get(uid)
        .map(|e| e.value().frozen_micro.load(Ordering::Acquire))
        .unwrap_or(0)
}

#[test]
fn release_zeros_frozen_and_leaves_deducted_alone() {
    let uid = UserId::from("freeze_proxy_release");
    let before_frozen = frozen(&uid);
    let before_deducted = deducted(&uid);

    let guard = RequestFreeze::new(uid.clone(), 1_000).expect("valid request freeze");
    assert_eq!(frozen(&uid), before_frozen + 1_000);

    guard.release();
    assert_eq!(
        frozen(&uid),
        before_frozen,
        "release must remove the freeze"
    );
    assert_eq!(
        deducted(&uid),
        before_deducted,
        "Rust never writes deducted_micro"
    );
}

#[test]
fn drop_without_release_rolls_back_frozen() {
    let uid = UserId::from("freeze_proxy_drop");
    let before_frozen = frozen(&uid);

    {
        let _guard = RequestFreeze::new(uid.clone(), 500).expect("valid request freeze");
        assert_eq!(frozen(&uid), before_frozen + 500);
    }
    assert_eq!(frozen(&uid), before_frozen);
}

#[test]
fn double_release_is_idempotent() {
    let uid = UserId::from("freeze_proxy_double");
    let before_deducted = deducted(&uid);

    let guard = RequestFreeze::new(uid.clone(), 100).expect("valid request freeze");
    guard.release();
    guard.release();
    assert_eq!(deducted(&uid), before_deducted);
}

#[test]
fn trait_object_release_dispatches() {
    let uid = UserId::from("freeze_proxy_trait_obj");
    let g: Box<dyn FreezeGuard> =
        Box::new(RequestFreeze::new(uid.clone(), 250).expect("valid request freeze"));
    assert_eq!(g.freeze_amount(), 250);
    g.release();
    assert_eq!(frozen(&uid), 0);
}
