use gw_core::error::codes;
use gw_core::error::{AccountBlockReason, ClassifierOutcome, ErrorClass, Retryability};

#[test]
fn error_class_default_is_internal() {
    let oc = ClassifierOutcome::default();
    assert_eq!(oc.class, ErrorClass::Internal);
    assert_eq!(oc.retryable, Retryability::No);
    assert!(oc.cooldown.is_none());
    assert!(oc.rate_limit_hint.is_none());
    assert!(oc.oauth_signal.is_none());
    assert!(oc.passthrough_body.is_none());
}

#[test]
fn error_class_account_blocked_carries_reason() {
    let class = ErrorClass::AccountBlocked(AccountBlockReason::Status401AccountDeactivated);
    match class {
        ErrorClass::AccountBlocked(r) => {
            assert_eq!(r, AccountBlockReason::Status401AccountDeactivated)
        }
        _ => panic!(),
    }
}

#[test]
fn rate_limit_kind_use_only_no_redefine() {
    let _: ErrorClass = ErrorClass::RateLimitedUpstream(gw_core::ratelimit::RateLimitKind::Rpm);
}

#[test]
fn phase2_quota_codes_are_balance_and_cache_or_coef_specific() {
    assert_eq!(
        codes::GW_QUOTA_BALANCE_INSUFFICIENT,
        "GW_QUOTA_BALANCE_INSUFFICIENT"
    );
    assert_eq!(
        codes::GW_QUOTA_BALANCE_CACHE_UNAVAILABLE,
        "GW_QUOTA_BALANCE_CACHE_UNAVAILABLE"
    );
    assert_eq!(
        codes::GW_QUOTA_COEF_TABLE_NOT_READY,
        "GW_QUOTA_COEF_TABLE_NOT_READY"
    );
}
