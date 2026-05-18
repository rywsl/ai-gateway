//! FreezeGuard RAII semantics (pure unit; no Redis).

use gw_core::ids::UserId;
use gw_ledger::FreezeGuard;
use gw_store::deduction::DEDUCT;
use std::sync::atomic::Ordering;

#[test]
fn freeze_and_settle_balanced() {
    let uid = UserId::from("u-freeze");
    let g = FreezeGuard::freeze(uid.clone(), 1000);
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 1000);
    g.settle(800);
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 800);
}

#[test]
fn freeze_drop_without_settle_rolls_back() {
    let uid = UserId::from("u-leak");
    {
        let _g = FreezeGuard::freeze(uid.clone(), 500);
    }
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn double_settle_is_noop() {
    use gw_core::ids::UserId;
    use gw_ledger::FreezeGuard;
    use gw_store::deduction::DEDUCT;
    use std::sync::atomic::Ordering;

    let uid = UserId::from("u-double-settle");
    let g = FreezeGuard::freeze(uid.clone(), 1000);
    g.settle(500);
    g.settle(999); // should be no-op
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 500); // first call won
}

#[test]
fn settle_then_drop_no_rollback() {
    use gw_core::ids::UserId;
    use gw_ledger::FreezeGuard;
    use gw_store::deduction::DEDUCT;
    use std::sync::atomic::Ordering;

    let uid = UserId::from("u-settle-then-drop");
    {
        let g = FreezeGuard::freeze(uid.clone(), 800);
        g.settle(600);
    } // drop runs, but settled=true so should skip rollback
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 600);
}
