//! `UpstreamErrorAnalyzer` — vendor-aware classification of upstream
//! responses into `ErrorOutcome` (full K-B impl in Phase 3).
//!
//! Phase 1d lands the trait surface + a `DefaultErrorAnalyzer` that
//! returns "uncategorized" for everything. Phase 3 fills in the vendor
//! analyzers (claude_code 401, gemini_cli quota header, codex retry-after,
//! …) and the rate-limit info extraction.

/// Outcome of analyzing an upstream HTTP response.
///
/// Phase 1d places this in `gw-adapter` so the trait can compile; Phase 3
/// K-B may move it to `gw-health` if it grows fields shared with the
/// health store. Either crate is allowed per integration spec §3 C-7.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ErrorOutcome {
    /// `None` = uncategorized. Phase 3 introduces the full enum.
    pub kind: Option<ErrorKind>,
    /// Retry-After hint from the upstream (seconds).
    pub retry_after_secs: Option<u32>,
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

impl UpstreamErrorAnalyzer for DefaultErrorAnalyzer {
    fn analyze(&self, status: u16, _body_head: &[u8], headers: &dyn HeaderView) -> ErrorOutcome {
        // Phase 1d skeleton: only the universally-safe Retry-After header
        // is parsed. Vendor-specific extraction (gemini quota headers,
        // codex headers, etc.) lands in Phase 3.
        let retry_after_secs = headers.get("retry-after").and_then(|s| s.parse::<u32>().ok());
        let kind = match status {
            401 | 403 => Some(ErrorKind::Authentication),
            429 => Some(ErrorKind::RateLimit),
            500..=599 => Some(ErrorKind::UpstreamServer),
            _ => None,
        };
        ErrorOutcome {
            kind,
            retry_after_secs,
        }
    }
}
