//! Per-key in-flight counter semantics (extracted from the old breaker tests).

use gw_core::ids::KeyId;
use gw_routing::in_flight::{InFlightCounter, InFlightGuard, InFlightStore};
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[test]
fn counter_starts_at_zero() {
    let s = InFlightStore::default();
    assert_eq!(s.count(&KeyId::from("k-fresh")), 0);
}

#[test]
fn guard_bumps_then_releases_on_drop() {
    let s = InFlightStore::default();
    let k = KeyId::from("k-guard");
    {
        let _g = InFlightGuard::new(s.get_or_create(&k));
        assert_eq!(s.count(&k), 1);
    }
    assert_eq!(s.count(&k), 0);
}

#[test]
fn concurrent_guards_sum_correctly() {
    let s = InFlightStore::default();
    let k = KeyId::from("k-concurrent");
    let _g1 = InFlightGuard::new(s.get_or_create(&k));
    let _g2 = InFlightGuard::new(s.get_or_create(&k));
    let _g3 = InFlightGuard::new(s.get_or_create(&k));
    assert_eq!(s.count(&k), 3);
}

#[test]
fn counter_is_a_proper_arc_handle() {
    let s = InFlightStore::default();
    let k = KeyId::from("k-arc");
    let h1 = s.get_or_create(&k);
    let h2 = s.get_or_create(&k);
    // Both handles point to the same atomic.
    assert!(Arc::ptr_eq(&h1, &h2));
    h1.count.fetch_add(5, Ordering::AcqRel);
    assert_eq!(h2.count.load(Ordering::Relaxed), 5);
    // Reset for cleanliness.
    h1.count.store(0, Ordering::Release);
    let _ = InFlightCounter::default();
}
