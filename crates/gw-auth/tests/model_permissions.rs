use gw_auth::model_permissions::{
    check_model_against_permissions, compile_pattern, ModelPermission, PermissionEffect,
};
use gw_runtime::{RegexCache, RegexCacheConfig};

fn make_cache() -> RegexCache {
    RegexCache::new(RegexCacheConfig::default())
}

#[test]
fn allow_then_deny_specific() {
    let cache = make_cache();
    let p = vec![
        ModelPermission::new(&cache, "^claude-.*$", PermissionEffect::Allow).unwrap(),
        ModelPermission::new(&cache, "^claude-3-haiku.*$", PermissionEffect::Deny).unwrap(),
    ];
    assert!(check_model_against_permissions("claude-3-opus", &p));
    assert!(!check_model_against_permissions(
        "claude-3-haiku-20240307",
        &p
    ));
}

#[test]
fn no_match_default_deny() {
    let cache = make_cache();
    let p = vec![ModelPermission::new(&cache, "^gpt-4.*$", PermissionEffect::Allow).unwrap()];
    assert!(!check_model_against_permissions("claude-3-opus", &p));
}

#[test]
fn empty_perms_default_deny() {
    assert!(!check_model_against_permissions("anything", &[]));
}

#[test]
fn deny_wins_when_both_match() {
    let cache = make_cache();
    let p = vec![
        ModelPermission::new(&cache, "^.*$", PermissionEffect::Allow).unwrap(),
        ModelPermission::new(&cache, "^claude-3-haiku.*$", PermissionEffect::Deny).unwrap(),
    ];
    assert!(!check_model_against_permissions("claude-3-haiku-X", &p));
    // any other claude is allowed by the wildcard
    assert!(check_model_against_permissions("claude-3-opus", &p));
}

#[test]
fn compile_rejects_overlong_pattern() {
    let big_pattern = "x".repeat(2000);
    assert!(
        compile_pattern(&big_pattern).is_err(),
        "expected MAX_PATTERN_LEN guard"
    );
}

#[test]
fn compile_accepts_normal_patterns() {
    assert!(compile_pattern("^claude-3-.*$").is_ok());
    assert!(compile_pattern("^gpt-(3|4)").is_ok());
    assert!(compile_pattern(".*").is_ok());
}

#[test]
fn compile_rejects_invalid_regex() {
    // Unbalanced bracket
    assert!(compile_pattern("[abc").is_err());
}

#[test]
fn new_rejects_overlong_pattern_via_cache() {
    let cache = make_cache();
    let big_pattern = "x".repeat(2000);
    assert!(
        ModelPermission::new(&cache, &big_pattern, PermissionEffect::Allow).is_err(),
        "expected MAX_PATTERN_LEN guard via ModelPermission::new"
    );
}
