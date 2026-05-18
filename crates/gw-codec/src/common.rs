//! Shared helpers used by every protocol codec — error builders, content
//! flattening, token approximation, error-status classification.

use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ir::{ContentIR, ContentPart};
use std::borrow::Cow;

#[inline(always)]
pub(crate) fn internal_err(msg: &'static str) -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::GatewayInternal,
        status: 500,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_GATEWAY_INTERNAL),
        message: Cow::Borrowed(msg),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}

#[inline(always)]
pub(crate) fn bad_request_err(msg: String) -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientBadRequest,
        status: 400,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_REQUEST_PROTOCOL_UNKNOWN),
        message: Cow::Owned(msg),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}

#[inline(always)]
pub(crate) fn classify(status: u16) -> ErrorCategory {
    match status {
        429 => ErrorCategory::ClientQuota,
        400..=499 => ErrorCategory::ClientBadRequest,
        500..=599 => ErrorCategory::UpstreamError,
        _ => ErrorCategory::GatewayInternal,
    }
}

#[inline(always)]
pub(crate) fn map_code(status: u16) -> &'static str {
    match status {
        // 429 from an *upstream* indicates the vendor itself is overloaded /
        // rate-limiting us, distinct from the gateway tripping a user's
        // per-account RPM cap (which uses GW_QUOTA_RPM_EXCEEDED). Keep the
        // codes distinct so dashboards and on-call runbooks can tell which
        // ceiling fired.
        429 => codes::GW_UPSTREAM_OVERLOADED,
        400..=499 => codes::GW_UPSTREAM_4XX,
        500..=599 => codes::GW_UPSTREAM_5XX,
        _ => codes::GW_GATEWAY_INTERNAL,
    }
}

/// Flatten a `ContentIR` (Text or Parts) to a single `Cow<str>`. For text
/// parts of a `Parts` content we join with empty separator. Non-text parts
/// (image / audio / file) are dropped — v1 protocols here only round-trip
/// the text channel.
#[inline]
pub(crate) fn content_to_text<'a>(c: &ContentIR<'a>) -> Cow<'a, str> {
    match c {
        ContentIR::Text(t) => t.clone(),
        ContentIR::Parts(parts) => {
            let mut buf = String::new();
            for p in parts {
                if let ContentPart::Text(t) = p {
                    buf.push_str(t);
                }
            }
            Cow::Owned(buf)
        }
    }
}

/// Approx 4 chars per token, minimum 1 — adequate for cheap pre-flight
/// budget checks; the upstream's actual `usage` always supersedes this.
#[inline]
pub(crate) fn approx_tokens(c: &ContentIR<'_>) -> u32 {
    match c {
        ContentIR::Text(t) => (t.chars().count() as u32 / 4).max(1),
        ContentIR::Parts(parts) => parts
            .iter()
            .map(|p| match p {
                ContentPart::Text(t) => (t.chars().count() as u32 / 4).max(1),
                _ => 0,
            })
            .sum(),
    }
}
