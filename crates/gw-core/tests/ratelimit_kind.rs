use gw_core::error::HeadersAccessor;
use gw_core::ratelimit::{detect_rate_limit_kind, RateLimitKind};
use std::collections::HashMap;

#[derive(Default)]
struct Headers(HashMap<&'static str, &'static str>);

impl HeadersAccessor for Headers {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).copied()
    }
}

fn headers(items: &[(&'static str, &'static str)]) -> Headers {
    Headers(items.iter().copied().collect())
}

#[test]
fn detects_rpm_from_request_rate_headers() {
    let h = headers(&[("anthropic-ratelimit-requests-limit", "60")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Rpm);

    let h = headers(&[("x-ratelimit-remaining-requests", "0")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Rpm);
}

#[test]
fn detects_tpm_from_token_headers() {
    let h = headers(&[("anthropic-ratelimit-tokens-remaining", "0")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Tpm);

    let h = headers(&[("x-ratelimit-limit-tokens", "1000")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Tpm);
}

#[test]
fn detects_window_kinds_from_payload_text() {
    let daily = serde_json::json!({"error": {"message": "daily quota exceeded"}});
    let monthly = serde_json::json!({"error": {"message": "monthly usage limit reached"}});
    let concurrent = serde_json::json!({"error": {"message": "too many concurrent requests"}});

    assert_eq!(
        detect_rate_limit_kind(&Headers::default(), Some(&daily)),
        RateLimitKind::Daily
    );
    assert_eq!(
        detect_rate_limit_kind(&Headers::default(), Some(&monthly)),
        RateLimitKind::Monthly
    );
    assert_eq!(
        detect_rate_limit_kind(&Headers::default(), Some(&concurrent)),
        RateLimitKind::Concurrent
    );
}

#[test]
fn unknown_when_no_specific_signal_exists() {
    let h = headers(&[("x-ratelimit-limit", "60")]);
    assert_eq!(detect_rate_limit_kind(&h, None), RateLimitKind::Unknown);
}
