//! G3 (T2-A) — pure-function tests for the daily-quota gate.
//!
//! Spec: ADR-G3-1, G3-2. `check_daily_quota` is intentionally a pure helper
//! so it can be unit-tested without standing up Redis. The Redis-backed
//! integration runs at the proxy E2E level.

use gw_proxy::filters::request::check_daily_quota;

#[test]
fn zero_means_unlimited() {
    assert!(check_daily_quota(0, 0, 0).is_ok());
    assert!(check_daily_quota(1_000_000_000, 1_000, 0).is_ok());
}

#[test]
fn under_quota_passes() {
    assert!(check_daily_quota(50_000, 500, 100_000).is_ok());
}

#[test]
fn exactly_at_quota_passes() {
    // boundary: consumed + freeze == quota is NOT over.
    assert!(check_daily_quota(99_000, 1_000, 100_000).is_ok());
}

#[test]
fn over_quota_rejects() {
    assert!(check_daily_quota(99_500, 1_000, 100_000).is_err());
}

#[test]
fn saturating_add_does_not_panic() {
    // Pathological estimate must not overflow.
    assert!(check_daily_quota(u64::MAX - 5, u64::MAX - 5, 100_000).is_err());
}
