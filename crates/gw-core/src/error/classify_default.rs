//! Default classifier — hard-coded HTTP status → ClassifierOutcome mapping.
//!
//! Used as the fall-through when no vendor-specific K-B analyzer is registered
//! for the upstream key. Vendor adapters may override per-vendor by giving
//! their own `ErrorClassifier` impl.

use super::class::{
    AccountBlockReason, ClassifierOutcome, ClassifyCtx, CooldownDirective, ErrorClass,
    ErrorClassifier, HeadersAccessor, OAuthSignal, OAuthSignalKind, RateLimitHint, Retryability,
};
use super::ir::ErrorIR;
use super::ErrorCategory;
use crate::ratelimit::RateLimitKind;

pub struct DefaultClassifier;

impl ErrorClassifier for DefaultClassifier {
    fn classify_local(&self, err: &ErrorIR<'_>) -> ClassifierOutcome {
        let class = match err.category {
            ErrorCategory::ClientAuth => ErrorClass::AuthDenied,
            ErrorCategory::ClientQuota => ErrorClass::QuotaDenied,
            ErrorCategory::ClientBadRequest => ErrorClass::ProtocolError,
            ErrorCategory::UpstreamTimeout | ErrorCategory::UpstreamError => {
                ErrorClass::UpstreamTransient
            }
            ErrorCategory::UpstreamOverloaded => {
                ErrorClass::RateLimitedUpstream(RateLimitKind::Unknown)
            }
            ErrorCategory::Cancelled => ErrorClass::ClientAbort,
            ErrorCategory::GatewayInternal => ErrorClass::Internal,
        };
        ClassifierOutcome {
            class,
            retryable: Retryability::No,
            ..Default::default()
        }
    }

    fn classify_upstream(
        &self,
        _ctx: &ClassifyCtx<'_>,
        http_status: u16,
        headers: &dyn HeadersAccessor,
        _vendor_payload: Option<&serde_json::Value>,
    ) -> ClassifierOutcome {
        let retry_after = headers
            .get("retry-after")
            .and_then(|s| s.parse::<u32>().ok());
        match http_status {
            401 => ClassifierOutcome {
                class: ErrorClass::AccountBlocked(AccountBlockReason::Status401Unauthorized),
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 2,
                    ttl_secs: retry_after.unwrap_or(3600),
                    reset_consecutive: false,
                }),
                oauth_signal: Some(OAuthSignal {
                    kind: OAuthSignalKind::AccountBlocked,
                }),
                ..Default::default()
            },
            402 => ClassifierOutcome {
                class: ErrorClass::AccountBlocked(AccountBlockReason::Status402PaymentRequired),
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 3,
                    ttl_secs: 3600,
                    reset_consecutive: false,
                }),
                ..Default::default()
            },
            403 => ClassifierOutcome {
                class: ErrorClass::AccountBlocked(AccountBlockReason::Status403AccountSuspended),
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 4,
                    ttl_secs: 1800,
                    reset_consecutive: false,
                }),
                ..Default::default()
            },
            408 => ClassifierOutcome {
                class: ErrorClass::UpstreamTransient,
                retryable: Retryability::SameKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 10,
                    ttl_secs: 30,
                    reset_consecutive: false,
                }),
                ..Default::default()
            },
            429 => ClassifierOutcome {
                class: ErrorClass::RateLimitedUpstream(RateLimitKind::Unknown),
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 7,
                    ttl_secs: retry_after.unwrap_or(60),
                    reset_consecutive: false,
                }),
                rate_limit_hint: Some(RateLimitHint {
                    kind: RateLimitKind::Unknown,
                    retry_after_secs: retry_after,
                }),
                ..Default::default()
            },
            529 => ClassifierOutcome {
                class: ErrorClass::UpstreamTransient,
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 8,
                    ttl_secs: 30,
                    reset_consecutive: false,
                }),
                ..Default::default()
            },
            500..=599 => ClassifierOutcome {
                class: ErrorClass::UpstreamTransient,
                retryable: Retryability::SameKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 9,
                    ttl_secs: 30,
                    reset_consecutive: false,
                }),
                ..Default::default()
            },
            _ => ClassifierOutcome {
                class: ErrorClass::ProtocolError,
                retryable: Retryability::No,
                ..Default::default()
            },
        }
    }
}
