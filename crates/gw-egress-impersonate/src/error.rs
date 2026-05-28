//! Map wreq errors → `X-Egress-Error-Class` header values.
//! The gw-proxy response_filter pattern-matches these strings.
//!
//! # API Notes vs Plan
//! - wreq 6.0.0-rc.28 DOES have `is_tls()`, `is_connect()`, `is_timeout()`,
//!   `is_body()`, `is_decode()` predicates — identical to reqwest's interface.
//! - `is_tls()` is used directly for TLS errors instead of string heuristics.
//! - `is_proxy_connect()` is used for proxy errors (wreq-specific predicate).

pub const HDR_CLASS: &str = "X-Egress-Error-Class";
pub const HDR_DETAIL: &str = "X-Egress-Error-Detail";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    Tls,
    ConnectTimeout,
    ConnectRefused,
    ReadTimeout,
    H2Protocol,
    BodyTruncated,
    ProxyError,
    Panic,
    Unknown,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::ConnectTimeout => "connect_timeout",
            Self::ConnectRefused => "connect_refused",
            Self::ReadTimeout => "read_timeout",
            Self::H2Protocol => "h2_protocol",
            Self::BodyTruncated => "body_truncated",
            Self::ProxyError => "proxy_error",
            Self::Panic => "panic",
            Self::Unknown => "unknown",
        }
    }

    pub fn status_code(self) -> u16 {
        match self {
            Self::ConnectTimeout | Self::ReadTimeout => 504,
            _ => 502,
        }
    }
}

/// Classify a wreq error into a stable `ErrorClass` value for header emission.
///
/// Detection order matters: `is_tls()` is checked before `is_connect()` because
/// TLS handshake failures set both flags in wreq.
pub fn classify(err: &wreq::Error) -> ErrorClass {
    if err.is_tls() {
        return ErrorClass::Tls;
    }
    if err.is_timeout() {
        return ErrorClass::ConnectTimeout;
    }
    if err.is_proxy_connect() {
        return ErrorClass::ProxyError;
    }
    if err.is_connect() {
        // Belt-and-suspenders: is_proxy_connect() above catches the wreq-specific
        // proxy-connect variant, but some upstream proxy auth/handshake failures
        // surface only via Display containing "proxy". Keep this fallback until
        // wreq exposes a finer predicate.
        let s = err.to_string();
        if s.contains("proxy") {
            return ErrorClass::ProxyError;
        }
        return ErrorClass::ConnectRefused;
    }
    if err.is_body() {
        return ErrorClass::BodyTruncated;
    }
    if err.is_decode() {
        return ErrorClass::H2Protocol;
    }
    ErrorClass::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_strings_are_stable() {
        // These exact strings are gw-proxy's pattern-match contract. Pin them.
        assert_eq!(ErrorClass::Tls.as_str(), "tls");
        assert_eq!(ErrorClass::ConnectTimeout.as_str(), "connect_timeout");
        assert_eq!(ErrorClass::ConnectRefused.as_str(), "connect_refused");
        assert_eq!(ErrorClass::ReadTimeout.as_str(), "read_timeout");
        assert_eq!(ErrorClass::H2Protocol.as_str(), "h2_protocol");
        assert_eq!(ErrorClass::BodyTruncated.as_str(), "body_truncated");
        assert_eq!(ErrorClass::ProxyError.as_str(), "proxy_error");
        assert_eq!(ErrorClass::Panic.as_str(), "panic");
        assert_eq!(ErrorClass::Unknown.as_str(), "unknown");
    }

    #[test]
    fn status_codes_match_class() {
        assert_eq!(ErrorClass::ConnectTimeout.status_code(), 504);
        assert_eq!(ErrorClass::ReadTimeout.status_code(), 504);
        assert_eq!(ErrorClass::Tls.status_code(), 502);
        assert_eq!(ErrorClass::ConnectRefused.status_code(), 502);
        assert_eq!(ErrorClass::H2Protocol.status_code(), 502);
        assert_eq!(ErrorClass::BodyTruncated.status_code(), 502);
        assert_eq!(ErrorClass::ProxyError.status_code(), 502);
        assert_eq!(ErrorClass::Panic.status_code(), 502);
        assert_eq!(ErrorClass::Unknown.status_code(), 502);
    }
}
