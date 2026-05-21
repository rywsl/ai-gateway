use gw_auth::model_permissions::{
    check_model_against_permissions, compile_pattern, ModelPermission, PermissionEffect,
};

#[test]
fn allow_then_deny_specific() {
    let p = vec![
        ModelPermission::new("^claude-.*$", PermissionEffect::Allow).unwrap(),
        ModelPermission::new("^claude-3-haiku.*$", PermissionEffect::Deny).unwrap(),
    ];
    assert!(check_model_against_permissions("claude-3-opus", &p));
    assert!(!check_model_against_permissions("claude-3-haiku-20240307", &p));
}

#[test]
fn no_match_default_deny() {
    let p = vec![ModelPermission::new("^gpt-4.*$", PermissionEffect::Allow).unwrap()];
    assert!(!check_model_against_permissions("claude-3-opus", &p));
}

#[test]
fn empty_perms_default_deny() {
    assert!(!check_model_against_permissions("anything", &[]));
}

#[test]
fn deny_wins_when_both_match() {
    let p = vec![
        ModelPermission::new("^.*$", PermissionEffect::Allow).unwrap(),
        ModelPermission::new("^claude-3-haiku.*$", PermissionEffect::Deny).unwrap(),
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
