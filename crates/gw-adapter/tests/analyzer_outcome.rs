use gw_adapter::{DefaultErrorAnalyzer, ErrorOutcome, HeaderView, UpstreamErrorAnalyzer};
use gw_auth::ratelimit::adaptive::{AdaptiveCfg, AdaptiveRpmManager};
use gw_core::ids::KeyId;
use std::sync::Arc;

struct DummyHeaders;
impl HeaderView for DummyHeaders {
    fn get(&self, _: &str) -> Option<&str> {
        None
    }
}

#[test]
fn outcome_has_cooldown_enhanced_body_oauth_signal() {
    let oc = ErrorOutcome::default();
    assert!(oc.cooldown.is_none());
    assert!(oc.enhanced_body.is_none());
    assert!(oc.oauth_signal.is_none());
}

#[test]
fn default_analyzer_401_yields_oauth_signal_none_until_vendor() {
    let a = DefaultErrorAnalyzer;
    let oc = a.analyze(401, b"", &DummyHeaders);
    // Default 不知道是不是 oauth；vendor 子类才决定
    assert!(oc.oauth_signal.is_none());
}

#[test]
fn test_error_analyzer_calls_observe_429() {
    struct Headers;
    impl HeaderView for Headers {
        fn get(&self, name: &str) -> Option<&str> {
            match name {
                "x-ratelimit-limit" => Some("120"),
                _ => None,
            }
        }
    }

    let cfg = AdaptiveCfg {
        min_header_observations: 1,
        ..AdaptiveCfg::default()
    };
    let adaptive = Arc::new(AdaptiveRpmManager::new(cfg));
    let key_id = KeyId::from("vk-analyzer-429");

    let a = DefaultErrorAnalyzer;
    let outcome =
        a.analyze_with_adaptive_rpm(429, b"", &Headers, adaptive.clone(), &key_id, 1_000, 333);

    assert!(matches!(
        outcome.kind,
        Some(gw_adapter::ErrorKind::RateLimit)
    ));
    assert_eq!(adaptive.should_enforce(&key_id), Some(102));
}
