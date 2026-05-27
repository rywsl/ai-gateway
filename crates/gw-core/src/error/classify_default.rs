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

/// Config-driven classifier that overrides `Retryability` based on a
/// caller-supplied set of retryable HTTP status codes.
pub struct ConfigurableClassifier {
    retryable_codes: std::collections::HashSet<u16>,
    inner: DefaultClassifier,
}

impl ConfigurableClassifier {
    /// Build from a set of retryable HTTP status codes. Default set
    /// `{429, 500, 502, 503, 504}` matches legacy hardcoded behaviour.
    pub fn new(retryable_codes: std::collections::HashSet<u16>) -> Self {
        Self {
            retryable_codes,
            inner: DefaultClassifier,
        }
    }

    pub fn retryable_codes(&self) -> &std::collections::HashSet<u16> {
        &self.retryable_codes
    }
}

impl ErrorClassifier for ConfigurableClassifier {
    fn classify_local(&self, err: &ErrorIR<'_>) -> ClassifierOutcome {
        self.inner.classify_local(err)
    }

    fn classify_upstream(
        &self,
        ctx: &ClassifyCtx<'_>,
        http_status: u16,
        headers: &dyn HeadersAccessor,
        vendor_payload: Option<&serde_json::Value>,
    ) -> ClassifierOutcome {
        let mut outcome = self
            .inner
            .classify_upstream(ctx, http_status, headers, vendor_payload);
        // Only override retryability for statuses in the failover-eligible
        // range (429 + 5xx). Other statuses (401 account-blocked, etc.) keep
        // DefaultClassifier's retryability judgment unchanged.
        let is_failover_range = http_status == 429 || (500..=599).contains(&http_status);
        if is_failover_range {
            if self.retryable_codes.contains(&http_status) {
                if outcome.retryable == Retryability::No {
                    outcome.retryable = Retryability::CrossKey;
                }
            } else {
                outcome.retryable = Retryability::No;
            }
        }
        outcome
    }
}

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
        ctx: &ClassifyCtx<'_>,
        http_status: u16,
        headers: &dyn HeadersAccessor,
        _vendor_payload: Option<&serde_json::Value>,
    ) -> ClassifierOutcome {
        let retry_after = headers
            .get("retry-after")
            .and_then(|s| s.parse::<u32>().ok());
        match http_status {
            401 if ctx.has_oauth => ClassifierOutcome {
                class: ErrorClass::AccountBlocked(AccountBlockReason::OAuthRefreshFailed),
                retryable: Retryability::CrossKey,
                cooldown: Some(CooldownDirective {
                    reason_code: 12,
                    ttl_secs: retry_after.unwrap_or(300),
                    reset_consecutive: false,
                }),
                oauth_signal: Some(OAuthSignal {
                    kind: OAuthSignalKind::RefreshFailed,
                }),
                ..Default::default()
            },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ProtocolKind;

    fn make_ctx() -> ClassifyCtx<'static> {
        ClassifyCtx {
            key_id: "k1",
            vendor_id: "v1",
            api_format: ProtocolKind::OpenAi,
            protocol: ProtocolKind::OpenAi,
            has_oauth: false,
        }
    }

    struct EmptyHeaders;
    impl HeadersAccessor for EmptyHeaders {
        fn get(&self, _name: &str) -> Option<&str> {
            None
        }
    }

    fn default_codes() -> std::collections::HashSet<u16> {
        [429, 500, 502, 503, 504].into_iter().collect()
    }

    #[test]
    fn default_config_preserves_non_failover_retryability() {
        let cfg = ConfigurableClassifier::new(default_codes());
        let def = DefaultClassifier;
        let ctx = make_ctx();

        for status in [200, 400, 401, 402, 403, 408] {
            let cfg_out = cfg.classify_upstream(&ctx, status, &EmptyHeaders, None);
            let def_out = def.classify_upstream(&ctx, status, &EmptyHeaders, None);
            assert_eq!(
                cfg_out.retryable, def_out.retryable,
                "mismatch on status {status}: cfg={:?} def={:?}",
                cfg_out.retryable, def_out.retryable,
            );
        }
    }

    #[test]
    fn default_config_retries_configured_codes() {
        let cfg = ConfigurableClassifier::new(default_codes());
        let ctx = make_ctx();

        for status in [429, 500, 502, 503, 504] {
            let out = cfg.classify_upstream(&ctx, status, &EmptyHeaders, None);
            assert!(
                out.retryable != Retryability::No,
                "status {status} should be retryable"
            );
        }
    }

    #[test]
    fn default_config_demotes_unconfigured_5xx() {
        let cfg = ConfigurableClassifier::new(default_codes());
        let ctx = make_ctx();

        for status in [501, 505, 529] {
            let out = cfg.classify_upstream(&ctx, status, &EmptyHeaders, None);
            assert_eq!(
                out.retryable,
                Retryability::No,
                "status {status} should NOT be retryable (not in default set)"
            );
        }
    }

    #[test]
    fn custom_codes_removes_500() {
        let codes: std::collections::HashSet<u16> = [429, 502].into_iter().collect();
        let cfg = ConfigurableClassifier::new(codes);
        let ctx = make_ctx();

        let out_500 = cfg.classify_upstream(&ctx, 500, &EmptyHeaders, None);
        assert_eq!(out_500.retryable, Retryability::No);

        let out_429 = cfg.classify_upstream(&ctx, 429, &EmptyHeaders, None);
        assert!(out_429.retryable != Retryability::No);

        let out_502 = cfg.classify_upstream(&ctx, 502, &EmptyHeaders, None);
        assert!(out_502.retryable != Retryability::No);
    }

    #[test]
    fn promotes_non_retryable_status_in_set() {
        // 501 is in 500-599 range but DefaultClassifier marks it SameKey.
        // Adding it to the set should keep it retryable.
        let codes: std::collections::HashSet<u16> = [501].into_iter().collect();
        let cfg = ConfigurableClassifier::new(codes);
        let ctx = make_ctx();

        let out = cfg.classify_upstream(&ctx, 501, &EmptyHeaders, None);
        assert!(out.retryable != Retryability::No);
    }
}
