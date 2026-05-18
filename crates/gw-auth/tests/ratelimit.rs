//! Tests for [`gw_auth::ratelimit`].

use gw_auth::ratelimit::RateLimitGuard;
use gw_core::ids::UserId;
use gw_core::permissions::MergedPermissions;
use std::collections::HashSet;

fn perm(rpm: u32) -> MergedPermissions {
    MergedPermissions {
        user_id: UserId::from("u1"),
        effective_user_id: UserId::from("u1"),
        client_ip: "127.0.0.1".parse().unwrap(),
        allowed_models: HashSet::new(),
        allowed_channels: HashSet::new(),
        allowed_groups: HashSet::new(),
        forced_model_group: None,
        rpm_limit: rpm,
        tpm_limit: 0,
        max_concurrent_streams: 100,
        markup_product: 1.0,
    }
}

#[test]
fn rpm_within_limit_passes() {
    let g = RateLimitGuard::new();
    let p = perm(60);
    for _ in 0..5 {
        g.check(&p).expect("5 of 60-per-min budget should pass");
    }
}

#[test]
fn stream_slot_rai_decrements_on_drop() {
    let g = RateLimitGuard::new();
    let uid = UserId::from("u1");
    {
        let _slot = g.check_stream_slot(&uid, 1).expect("first slot ok");
        // Second slot must be rejected (cap = 1).
        assert!(g.check_stream_slot(&uid, 1).is_err());
    }
    // After RAII drop, counter is back to 0, so re-acquire should succeed.
    let _slot = g
        .check_stream_slot(&uid, 1)
        .expect("slot reacquirable after drop");
}

#[test]
fn unlimited_max_allows_many_slots() {
    let g = RateLimitGuard::new();
    let uid = UserId::from("u1");
    // max = 0 means unlimited.
    let _s1 = g.check_stream_slot(&uid, 0).unwrap();
    let _s2 = g.check_stream_slot(&uid, 0).unwrap();
    let _s3 = g.check_stream_slot(&uid, 0).unwrap();
}
