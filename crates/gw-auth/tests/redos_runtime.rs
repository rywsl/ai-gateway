use gw_auth::model_permissions::{
    check_with_budget, ModelPermission, PermissionEffect, RedosTracker, BUDGET_MS,
};
use gw_runtime::{RegexCache, RegexCacheConfig};

fn make_cache() -> RegexCache {
    RegexCache::new(RegexCacheConfig::default())
}

#[test]
fn match_within_budget_returns_normal() {
    let tracker = RedosTracker::new();
    let cache = make_cache();
    let p = vec![ModelPermission::new(&cache, "^claude.*$", PermissionEffect::Allow).unwrap()];
    let r = check_with_budget("claude-3-opus", &p, "ak1", &tracker);
    assert_eq!(r, Some(true));
    assert_eq!(tracker.timeout_count("ak1"), 0);
}

#[test]
fn deny_within_budget_returns_false() {
    let tracker = RedosTracker::new();
    let cache = make_cache();
    let p = vec![ModelPermission::new(&cache, "^gpt.*$", PermissionEffect::Allow).unwrap()];
    let r = check_with_budget("claude-3-opus", &p, "ak1", &tracker);
    assert_eq!(r, Some(false));
}

#[test]
fn timeouts_disable_after_threshold() {
    let tracker = RedosTracker::new();
    for _ in 0..10 {
        tracker.record_timeout("ak-bad");
    }
    assert!(tracker.is_disabled("ak-bad"));
    let p = vec![];
    let r = check_with_budget("anything", &p, "ak-bad", &tracker);
    assert_eq!(r, None);
}

#[test]
fn timeouts_below_threshold_do_not_disable() {
    let tracker = RedosTracker::new();
    for _ in 0..5 {
        tracker.record_timeout("ak-marginal");
    }
    assert!(!tracker.is_disabled("ak-marginal"));
    assert_eq!(tracker.timeout_count("ak-marginal"), 5);
}

#[test]
fn budget_constant_is_100ms() {
    assert_eq!(BUDGET_MS, 100);
}

#[test]
fn separate_keys_isolated() {
    let tracker = RedosTracker::new();
    for _ in 0..10 {
        tracker.record_timeout("ak-bad");
    }
    assert!(tracker.is_disabled("ak-bad"));
    assert!(!tracker.is_disabled("ak-good"));
    assert_eq!(tracker.timeout_count("ak-good"), 0);
}
