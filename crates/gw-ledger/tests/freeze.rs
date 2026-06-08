//! RequestFreeze RAII semantics — release-only (C-γ §ADR-Cγ-2).
//!
//! Critically: `release` does NOT touch `deducted_micro`. Go is the sole
//! writer of `deducted_micro` (via the `usage:records` consumer + an
//! `events:counter` echo back to Rust).

use gw_core::ids::UserId;
use gw_ledger::flusher::install_frozen_delta_sender_for_tests;
use gw_ledger::{FreezeGuard, FreezeKind, RequestFreeze, TaskFreeze};
use gw_store::deduction::DEDUCT;
use std::sync::atomic::Ordering;

#[test]
fn freeze_then_release_zeroes_frozen() {
    let uid = UserId::from("u-freeze-release");
    let g = RequestFreeze::new(uid.clone(), 1000);
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 1000);
    g.release();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    // Critical invariant: Rust never writes deducted_micro.
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn drop_without_release_rolls_back_frozen() {
    let uid = UserId::from("u-freeze-drop");
    {
        let _g = RequestFreeze::new(uid.clone(), 500);
    }
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn double_release_is_idempotent() {
    let uid = UserId::from("u-freeze-double-release");
    let g = RequestFreeze::new(uid.clone(), 1000);
    g.release();
    g.release(); // no-op
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn release_then_drop_does_not_double_rollback() {
    let uid = UserId::from("u-freeze-release-then-drop");
    {
        let g = RequestFreeze::new(uid.clone(), 800);
        g.release();
    }
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn trait_object_release_dispatches() {
    let uid = UserId::from("u-freeze-trait-obj");
    let g: Box<dyn FreezeGuard> = Box::new(RequestFreeze::new(uid.clone(), 100));
    assert_eq!(g.freeze_amount(), 100);
    g.release();
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn task_freeze_without_releaser_is_idempotent_for_tests() {
    let uid = UserId::from("u-task-freeze-no-releaser");
    let g = TaskFreeze::new(uid.clone(), "task-no-releaser", 1000, None);
    assert_eq!(g.kind(), FreezeKind::Task);
    assert_eq!(g.uid(), &uid);
    assert_eq!(g.freeze_amount(), 1000);
    g.release();
    g.release();
}

#[tokio::test]
async fn request_freeze_enqueues_redis_frozen_deltas() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let _sender_guard = install_frozen_delta_sender_for_tests(tx);
    let uid = UserId::from("u-freeze-delta");

    let g = RequestFreeze::new(uid.clone(), 700);
    let first = rx.recv().await.unwrap();
    assert_eq!(first.uid, uid);
    assert_eq!(first.delta_micro, 700);

    g.release();
    let second = rx.recv().await.unwrap();
    assert_eq!(second.uid, UserId::from("u-freeze-delta"));
    assert_eq!(second.delta_micro, -700);
}
