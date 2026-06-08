use gw_core::error::{
    AccountBlockReason, ClassifierOutcome, ClassifyCtx, DefaultClassifier, ErrorClass,
    ErrorClassifier, HeadersAccessor, OAuthSignalKind, Retryability,
};
use gw_core::protocol::ProtocolKind;
use gw_core::ratelimit::RateLimitKind;

struct H {
    rh: Option<&'static str>,
}
impl HeadersAccessor for H {
    fn get(&self, name: &str) -> Option<&str> {
        if name.eq_ignore_ascii_case("retry-after") {
            self.rh
        } else {
            None
        }
    }
}

struct TokenLimitHeaders;
impl HeadersAccessor for TokenLimitHeaders {
    fn get(&self, name: &str) -> Option<&str> {
        match name {
            "anthropic-ratelimit-tokens-remaining" => Some("0"),
            _ => None,
        }
    }
}

fn ctx() -> ClassifyCtx<'static> {
    ClassifyCtx {
        key_id: "k1",
        vendor_id: "v1",
        api_format: ProtocolKind::OpenAi,
        protocol: ProtocolKind::OpenAi,
        has_oauth: false,
    }
}

fn ctx_oauth() -> ClassifyCtx<'static> {
    ClassifyCtx {
        has_oauth: true,
        ..ctx()
    }
}

#[test]
fn upstream_429_with_retry_after_yields_rl_hint() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 429, &H { rh: Some("30") }, None);
    assert!(matches!(oc.class, ErrorClass::RateLimitedUpstream(_)));
    assert_eq!(oc.retryable, Retryability::CrossKey);
    let hint = oc.rate_limit_hint.as_ref().unwrap();
    assert_eq!(hint.retry_after_secs, Some(30));
    let cd = oc.cooldown.as_ref().unwrap();
    assert_eq!(cd.ttl_secs, 30);
}

#[test]
fn upstream_429_without_retry_after_uses_default_60s() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 429, &H { rh: None }, None);
    let cd = oc.cooldown.as_ref().unwrap();
    assert_eq!(cd.ttl_secs, 60);
}

#[test]
fn upstream_401_yields_account_blocked_with_oauth_signal() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 401, &H { rh: None }, None);
    match oc.class {
        ErrorClass::AccountBlocked(r) => {
            assert!(matches!(
                r,
                AccountBlockReason::Status401Unauthorized
                    | AccountBlockReason::Status401AccountDeactivated
            ));
        }
        _ => panic!("expected AccountBlocked, got {:?}", oc.class),
    }
    assert_eq!(oc.retryable, Retryability::CrossKey);
    let sig = oc.oauth_signal.as_ref().unwrap();
    assert!(matches!(sig.kind, OAuthSignalKind::AccountBlocked));
}

#[test]
fn upstream_401_with_oauth_yields_refresh_failed() {
    let oc = DefaultClassifier.classify_upstream(&ctx_oauth(), 401, &H { rh: None }, None);
    assert_eq!(
        oc.class,
        ErrorClass::AccountBlocked(AccountBlockReason::OAuthRefreshFailed)
    );
    assert_eq!(oc.retryable, Retryability::CrossKey);
    let sig = oc.oauth_signal.as_ref().unwrap();
    assert_eq!(sig.kind, OAuthSignalKind::RefreshFailed);
    let cd = oc.cooldown.as_ref().unwrap();
    assert_eq!(cd.reason_code, 12);
    assert_eq!(cd.ttl_secs, 300);
}

#[test]
fn upstream_401_with_oauth_respects_retry_after() {
    let oc = DefaultClassifier.classify_upstream(&ctx_oauth(), 401, &H { rh: Some("60") }, None);
    let cd = oc.cooldown.as_ref().unwrap();
    assert_eq!(cd.ttl_secs, 60);
    assert_eq!(cd.reason_code, 12);
}

#[test]
fn upstream_402_long_cooldown() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 402, &H { rh: None }, None);
    assert!(matches!(oc.class, ErrorClass::AccountBlocked(_)));
    assert_eq!(oc.cooldown.as_ref().unwrap().ttl_secs, 3600);
}

#[test]
fn upstream_403_medium_cooldown() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 403, &H { rh: None }, None);
    assert!(matches!(oc.class, ErrorClass::AccountBlocked(_)));
    assert_eq!(oc.cooldown.as_ref().unwrap().ttl_secs, 1800);
}

#[test]
fn upstream_408_transient_short_cooldown() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 408, &H { rh: None }, None);
    assert_eq!(oc.class, ErrorClass::UpstreamTransient);
    assert_eq!(oc.retryable, Retryability::SameKey);
    assert_eq!(oc.cooldown.as_ref().unwrap().ttl_secs, 30);
}

#[test]
fn upstream_503_transient() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 503, &H { rh: None }, None);
    assert_eq!(oc.class, ErrorClass::UpstreamTransient);
    assert_eq!(oc.retryable, Retryability::SameKey);
}

#[test]
fn upstream_529_overloaded() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 529, &H { rh: None }, None);
    assert_eq!(oc.class, ErrorClass::UpstreamTransient);
    assert_eq!(oc.retryable, Retryability::CrossKey);
}

#[test]
fn unknown_status_protocol_error() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 418, &H { rh: None }, None);
    assert_eq!(oc.class, ErrorClass::ProtocolError);
    assert_eq!(oc.retryable, Retryability::No);
}

#[test]
fn unused_param_rate_limit_kind_present() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 429, &H { rh: None }, None);
    let hint = oc.rate_limit_hint.as_ref().unwrap();
    assert_eq!(hint.kind, RateLimitKind::Unknown);
}

#[test]
fn upstream_429_rate_limit_hint_uses_detected_kind() {
    let oc = DefaultClassifier.classify_upstream(&ctx(), 429, &TokenLimitHeaders, None);
    assert_eq!(
        oc.class,
        ErrorClass::RateLimitedUpstream(RateLimitKind::Tpm)
    );
    let hint = oc.rate_limit_hint.as_ref().unwrap();
    assert_eq!(hint.kind, RateLimitKind::Tpm);
}

#[allow(dead_code)]
fn _unused_outcome_marker() -> ClassifierOutcome {
    ClassifierOutcome::default()
}
