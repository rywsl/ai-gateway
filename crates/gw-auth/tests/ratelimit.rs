//! Tests for [`gw_auth::ratelimit`].

use gw_auth::ratelimit::RateLimitGuard;
use gw_core::ids::UserId;
use gw_core::permissions::{MarkupChainSnapshot, MergedPermissions};
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
        markup_chain: MarkupChainSnapshot::identity(),
        model_whitelist_regex: None,
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

#[test]
fn tpm_under_limit_passes() {
    let g = RateLimitGuard::new();
    let uid = UserId::from("u1");
    let p = MergedPermissions {
        tpm_limit: 1000,
        ..perm(60)
    };
    g.deduct_tpm(&uid, &p, 500)
        .expect("500 of 1000 budget should pass");
}

#[test]
fn tpm_over_limit_rejects_with_correct_code() {
    let g = RateLimitGuard::new();
    let uid = UserId::from("u1");
    let p = MergedPermissions {
        tpm_limit: 100,
        ..perm(60)
    };
    let err = g.deduct_tpm(&uid, &p, 200).unwrap_err();
    assert_eq!(err.status, 429);
    assert_eq!(&*err.code, "GW_QUOTA_TPM_EXCEEDED");
    assert_eq!(err.retry_after_secs, Some(60));
}

#[test]
fn tpm_zero_limit_disables_enforcement() {
    let g = RateLimitGuard::new();
    let uid = UserId::from("u1");
    let p = perm(60); // tpm_limit = 0
    g.deduct_tpm(&uid, &p, 999_999)
        .expect("tpm_limit=0 means enforcement off");
}
