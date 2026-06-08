//! `UpstreamErrorAnalyzer` — vendor-aware classification of upstream
//! responses into `ErrorOutcome` (full K-B impl in Phase 3).
//!
//! Phase 1d lands the trait surface + a `DefaultErrorAnalyzer` that
//! returns "uncategorized" for everything. Phase 3 fills in the vendor
//! analyzers (claude_code 401, gemini_cli quota header, codex retry-after,
//! …) and the rate-limit info extraction.

use bytes::Bytes;
use gw_auth::ratelimit::adaptive::AdaptiveRpmManager;
use gw_core::error::{CooldownDirective, OAuthSignal};
use gw_core::ids::KeyId;
use std::sync::Arc;

/// Outcome of analyzing an upstream HTTP response.
///
/// Phase 1d places this in `gw-adapter` so the trait can compile; Phase 3
/// K-B may move it to `gw-health` if it grows fields shared with the
/// health store. Either crate is allowed per integration spec §3 C-7.
///
/// `Eq`/`PartialEq` is intentionally not derived: `Bytes` byte-equality
/// would force every comparison through full payload memcmp, and
/// `CooldownDirective`/`OAuthSignal` carry future fields we don't want
/// to commit to value-equality semantics for. Tests rely on `is_none()`
/// per field instead of struct equality.
#[derive(Clone, Debug, Default)]
pub struct ErrorOutcome {
    /// `None` = uncategorized. Phase 3 introduces the full enum.
    pub kind: Option<ErrorKind>,
    /// Retry-After hint from the upstream (seconds).
    pub retry_after_secs: Option<u32>,

    /// K-A 的冷却命令；adapter 看了 status + body 后决定。
    /// 由 § 1 ClassifierOutcome 透传 K-A apply_cooldown。
    pub cooldown: Option<CooldownDirective>,

    /// vendor 错误体 → 客户端协议错误体的 "vendor 端增强" 产物。
    /// kiro 等 vendor 在此处把帧头剥掉 / type 字段改写。
    /// 不做 vendor 改写时为 None（passthrough）。
    pub enhanced_body: Option<Bytes>,

    /// OAuth 标记：401 是 token 失效还是账号挂起？vendor 决定。
    pub oauth_signal: Option<OAuthSignal>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Authentication,
    RateLimit,
    Quota,
    UpstreamServer,
    Transient,
    Unknown,
}

pub trait UpstreamErrorAnalyzer: Send + Sync + 'static {
    /// Analyze an upstream response; return what the proxy / runtime
    /// should do next.
    fn analyze(&self, status: u16, body_head: &[u8], headers: &dyn HeaderView) -> ErrorOutcome;
}

/// Read-only header view passed to analyzers. Decoupled from the
/// adapter-side `HeadersAccessor` because analysis runs on responses;
/// the proxy / runtime supplies its own adapter at the call site.
pub trait HeaderView {
    fn get(&self, name: &str) -> Option<&str>;
}

pub struct DefaultErrorAnalyzer;

impl DefaultErrorAnalyzer {
    #[allow(clippy::too_many_arguments)]
    pub fn analyze_with_adaptive_rpm(
        &self,
        status: u16,
        body_head: &[u8],
        headers: &dyn HeaderView,
        adaptive_rpm: Arc<AdaptiveRpmManager>,
        key_id: &KeyId,
        configured_rpm: u32,
        current_local_rpm: u32,
    ) -> ErrorOutcome {
        let outcome = self.analyze(status, body_head, headers);
        if status == 429 {
            let header_limit = rate_limit_header(headers);
            let retry_after = retry_after_header(headers);
            adaptive_rpm.observe_429(
                key_id,
                configured_rpm,
                header_limit,
                current_local_rpm,
                retry_after,
            );
        }
        outcome
    }
}

fn rate_limit_header(headers: &dyn HeaderView) -> Option<u32> {
    [
        "x-ratelimit-limit",
        "x-rate-limit-limit",
        "ratelimit-limit",
        "rate-limit-limit",
    ]
    .into_iter()
    .find_map(|name| {
        headers
            .get(name)
            .and_then(|value| value.parse::<u32>().ok())
    })
}

fn retry_after_header(headers: &dyn HeaderView) -> Option<u32> {
    headers
        .get("retry-after")
        .and_then(|s| s.parse::<u32>().ok())
}

impl UpstreamErrorAnalyzer for DefaultErrorAnalyzer {
    fn analyze(&self, status: u16, _body_head: &[u8], headers: &dyn HeaderView) -> ErrorOutcome {
        // Phase 1d skeleton: only the universally-safe Retry-After header
        // is parsed. Vendor-specific extraction (gemini quota headers,
        // codex headers, etc.) lands in Phase 3.
        let retry_after_secs = headers
            .get("retry-after")
            .and_then(|s| s.parse::<u32>().ok());
        let kind = match status {
            401 | 403 => Some(ErrorKind::Authentication),
            429 => Some(ErrorKind::RateLimit),
            500..=599 => Some(ErrorKind::UpstreamServer),
            _ => None,
        };
        ErrorOutcome {
            kind,
            retry_after_secs,
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinAnalyzerKind {
    Codex,
    Anthropic,
    Gemini,
}

pub struct KeywordErrorAnalyzer {
    kind: BuiltinAnalyzerKind,
}

impl KeywordErrorAnalyzer {
    pub fn codex() -> Self {
        Self {
            kind: BuiltinAnalyzerKind::Codex,
        }
    }

    pub fn anthropic() -> Self {
        Self {
            kind: BuiltinAnalyzerKind::Anthropic,
        }
    }

    pub fn gemini() -> Self {
        Self {
            kind: BuiltinAnalyzerKind::Gemini,
        }
    }
}

fn body_contains_any(body_head: &[u8], needles: &[&str]) -> bool {
    let body = String::from_utf8_lossy(body_head).to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| body.contains(&needle.to_ascii_lowercase()))
}

fn cooldown(reason_code: u16, ttl_secs: u32, reset_consecutive: bool) -> CooldownDirective {
    CooldownDirective {
        reason_code,
        ttl_secs,
        reset_consecutive,
    }
}

impl UpstreamErrorAnalyzer for KeywordErrorAnalyzer {
    fn analyze(&self, status: u16, body_head: &[u8], headers: &dyn HeaderView) -> ErrorOutcome {
        let retry_after_secs = retry_after_header(headers);
        match self.kind {
            BuiltinAnalyzerKind::Codex => {
                if status == 429 {
                    let ttl = retry_after_secs.unwrap_or(60);
                    return ErrorOutcome {
                        kind: Some(ErrorKind::RateLimit),
                        retry_after_secs,
                        cooldown: Some(cooldown(7, ttl, false)),
                        ..Default::default()
                    };
                }
            }
            BuiltinAnalyzerKind::Anthropic => {
                if (status == 401 || status == 403)
                    && body_contains_any(body_head, &["disabled", "deactivated", "suspended"])
                {
                    return ErrorOutcome {
                        kind: Some(ErrorKind::Quota),
                        cooldown: Some(cooldown(13, 3600, false)),
                        ..Default::default()
                    };
                }
                if status == 429 {
                    let ttl = retry_after_secs.unwrap_or(60);
                    return ErrorOutcome {
                        kind: Some(ErrorKind::RateLimit),
                        retry_after_secs,
                        cooldown: Some(cooldown(7, ttl, false)),
                        ..Default::default()
                    };
                }
            }
            BuiltinAnalyzerKind::Gemini => {
                if status == 429
                    && body_contains_any(
                        body_head,
                        &["resource_exhausted", "quota exceeded", "quota"],
                    )
                {
                    let ttl = retry_after_secs.unwrap_or(60);
                    return ErrorOutcome {
                        kind: Some(ErrorKind::Quota),
                        retry_after_secs,
                        cooldown: Some(cooldown(7, ttl, false)),
                        ..Default::default()
                    };
                }
            }
        }
        DefaultErrorAnalyzer.analyze(status, body_head, headers)
    }
}

#[cfg(test)]
mod vendor_analyzer_tests {
    use super::*;

    struct Headers(Vec<(&'static str, &'static str)>);

    impl HeaderView for Headers {
        fn get(&self, name: &str) -> Option<&str> {
            self.0
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| *v)
        }
    }

    #[test]
    fn codex_429_reads_retry_after_and_marks_rate_limit() {
        let analyzer = KeywordErrorAnalyzer::codex();
        let out = analyzer.analyze(
            429,
            br#"{"error":{"message":"rate limit reached"}}"#,
            &Headers(vec![("Retry-After", "17")]),
        );
        assert_eq!(out.kind, Some(ErrorKind::RateLimit));
        assert_eq!(out.retry_after_secs, Some(17));
        let cooldown = out.cooldown.expect("429 produces cooldown");
        assert_eq!(cooldown.reason_code, 7);
        assert_eq!(cooldown.ttl_secs, 17);
    }

    #[test]
    fn anthropic_disabled_body_marks_account_blocked() {
        let analyzer = KeywordErrorAnalyzer::anthropic();
        let out = analyzer.analyze(
            403,
            br#"{"type":"error","error":{"message":"workspace account disabled"}}"#,
            &Headers(vec![]),
        );
        assert_eq!(out.kind, Some(ErrorKind::Quota));
        let cooldown = out.cooldown.expect("disabled account produces cooldown");
        assert_eq!(cooldown.reason_code, 13);
        assert_eq!(cooldown.ttl_secs, 3600);
    }

    #[test]
    fn gemini_resource_exhausted_body_marks_quota() {
        let analyzer = KeywordErrorAnalyzer::gemini();
        let out = analyzer.analyze(
            429,
            br#"{"error":{"status":"RESOURCE_EXHAUSTED","message":"quota exceeded"}}"#,
            &Headers(vec![]),
        );
        assert_eq!(out.kind, Some(ErrorKind::Quota));
        let cooldown = out.cooldown.expect("quota exhausted produces cooldown");
        assert_eq!(cooldown.reason_code, 7);
        assert_eq!(cooldown.ttl_secs, 60);
    }
}
