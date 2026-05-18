//! FreezeGuard lifecycle integration tests.
//!
//! These tests exercise the contract gw-proxy's logging filter relies on:
//! 1. `FreezeGuard::freeze` increments the per-user `DeductionState.frozen`.
//! 2. `settle(actual)` moves the frozen amount into `deducted` and zeroes
//!    `frozen` for that guard.
//! 3. Dropping the guard *without* `settle` rolls back `frozen` (the leak-
//!    recovered path) so a panicking filter never permanently leaks quota.
//!
//! Coverage overlaps with gw-ledger's own tests; we keep these here so any
//! future refactor that moves freeze handling into gw-proxy still has a local
//! regression net.

use gw_core::ids::UserId;
use gw_ledger::FreezeGuard;
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
fn settle_moves_frozen_into_deducted() {
    let uid = UserId::from("freeze_test_user_settle");
    let before_frozen = frozen(&uid);
    let before_deducted = deducted(&uid);

    let guard = FreezeGuard::freeze(uid.clone(), 1_000);
    assert_eq!(
        frozen(&uid),
        before_frozen + 1_000,
        "freeze should bump frozen by amount"
    );

    guard.settle(750);
    assert_eq!(
        frozen(&uid),
        before_frozen,
        "settle should remove the entire freeze"
    );
    assert_eq!(
        deducted(&uid),
        before_deducted + 750,
        "settle should add `actual` to deducted"
    );
}

#[test]
fn drop_without_settle_rolls_back_frozen() {
    let uid = UserId::from("freeze_test_user_drop");
    let before_frozen = frozen(&uid);

    {
        let _guard = FreezeGuard::freeze(uid.clone(), 500);
        assert_eq!(
            frozen(&uid),
            before_frozen + 500,
            "freeze should be visible while guard is alive"
        );
        // `_guard` is dropped here without settle.
    }
    assert_eq!(
        frozen(&uid),
        before_frozen,
        "drop without settle must roll the freeze back"
    );
}

#[test]
fn settle_is_idempotent() {
    let uid = UserId::from("freeze_test_user_idempotent");
    let before_deducted = deducted(&uid);

    let guard = FreezeGuard::freeze(uid.clone(), 100);
    guard.settle(80);
    guard.settle(999); // second settle is a no-op.
    assert_eq!(
        deducted(&uid),
        before_deducted + 80,
        "second settle must not double-add deduction"
    );
}
