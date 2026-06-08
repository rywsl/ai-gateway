use crate::error::HeadersAccessor;

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitKind {
    Rpm,
    Tpm,
    Daily,
    Monthly,
    Concurrent,
    Unknown,
}

pub fn detect_rate_limit_kind(
    headers: &dyn HeadersAccessor,
    vendor_payload: Option<&serde_json::Value>,
) -> RateLimitKind {
    if any_header(
        headers,
        &[
            "anthropic-ratelimit-tokens-limit",
            "anthropic-ratelimit-tokens-remaining",
            "x-ratelimit-limit-tokens",
            "x-ratelimit-remaining-tokens",
        ],
    ) {
        return RateLimitKind::Tpm;
    }
    if any_header(
        headers,
        &[
            "anthropic-ratelimit-requests-limit",
            "anthropic-ratelimit-requests-remaining",
            "x-ratelimit-limit-requests",
            "x-ratelimit-remaining-requests",
        ],
    ) {
        return RateLimitKind::Rpm;
    }

    let Some(payload) = vendor_payload else {
        return RateLimitKind::Unknown;
    };
    let text = payload.to_string().to_ascii_lowercase();
    if contains_any(&text, &["token", "tokens", "tpm"]) {
        RateLimitKind::Tpm
    } else if contains_any(&text, &["request per minute", "requests per minute", "rpm"]) {
        RateLimitKind::Rpm
    } else if contains_any(&text, &["daily", "per day", "day quota"]) {
        RateLimitKind::Daily
    } else if contains_any(&text, &["monthly", "per month", "month quota"]) {
        RateLimitKind::Monthly
    } else if contains_any(&text, &["concurrent", "in-flight", "in flight"]) {
        RateLimitKind::Concurrent
    } else {
        RateLimitKind::Unknown
    }
}

fn any_header(headers: &dyn HeadersAccessor, names: &[&str]) -> bool {
    names.iter().any(|name| headers.get(name).is_some())
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}
