//! HMAC-SHA256 request signature verification.
//!
//! NOTE: `timestamp` is included in the signing string but freshness is NOT
//! enforced here — the caller MUST validate the timestamp against a clock-skew
//! window (e.g. ±60 seconds) before invoking `verify` to prevent replay attacks.
//!
//! Canonical signing string is:
//! ```text
//! <method>\n<path>\n<timestamp>\n<sha256(body) as lowercase hex>
//! ```
//!
//! Comparison is constant-time via [`subtle::ConstantTimeEq`].

use hmac::{Hmac, Mac};
use moka::sync::Cache;
use sha2::Sha256;
use std::sync::Arc;
use subtle::ConstantTimeEq;

pub struct SignatureInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub timestamp: &'a str,
    pub body: &'a [u8],
}

pub fn verify(secret: &[u8], received: &[u8], input: &SignatureInput<'_>) -> bool {
    let body_hash = {
        use sha2::Digest;
        let mut h = Sha256::new();
        h.update(input.body);
        hex::encode(h.finalize())
    };
    let signing = format!(
        "{}\n{}\n{}\n{}",
        input.method, input.path, input.timestamp, body_hash
    );
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(signing.as_bytes());
    let expected = mac.finalize().into_bytes();
    expected.ct_eq(received).into()
}

use gw_core::entity::SignatureAlgo;
use std::time::Duration;

pub const REPLAY_WINDOW_SECS: i64 = 60;
pub const REPLAY_NONCE_TTL: Duration = Duration::from_secs(600);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SignatureCheckResult {
    /// Signature requirement is off — caller should pass through.
    NotRequired,
    /// `x-signature` or `x-timestamp` header absent when required. The client
    /// failed to satisfy a precondition (distinct from `Invalid`, which means
    /// the client sent headers but they were wrong).
    Missing,
    /// Timestamp outside the ±60 s replay window (in either direction).
    TimestampDrift,
    /// HMAC mismatch, or malformed timestamp string (header present but not
    /// parseable as an `i64`).
    Invalid,
    /// HMAC matches and timestamp is within the replay window.
    Valid,
    /// Signing is required but no `signature_secret` is configured (empty key).
    /// Fail-closed server misconfiguration: HMAC accepts a zero-length key, so
    /// verifying against it would let any client forge a valid signature. The
    /// caller MUST reject (HTTP 500) rather than treat this as a client error.
    SecretUnavailable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayCheckMode {
    #[default]
    None,
    NonceRedis,
    NonceLocal,
}

impl ReplayCheckMode {
    pub fn requires_nonce(self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Clone)]
pub struct ReplayChecker {
    mode: ReplayCheckMode,
    local: Cache<String, ()>,
    redis: Option<fred::clients::RedisPool>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReplayCheckError {
    #[error("signature nonce required")]
    MissingNonce,
    #[error("signature replay detected")]
    ReplayDetected,
    #[error("signature replay backend failed: {0}")]
    Backend(String),
}

impl ReplayChecker {
    pub fn new(mode: ReplayCheckMode, redis: Option<fred::clients::RedisPool>) -> Arc<Self> {
        Arc::new(Self {
            mode,
            local: Cache::builder()
                .max_capacity(100_000)
                .time_to_live(REPLAY_NONCE_TTL)
                .build(),
            redis,
        })
    }

    pub fn disabled() -> Arc<Self> {
        Self::new(ReplayCheckMode::None, None)
    }

    pub fn mode(&self) -> ReplayCheckMode {
        self.mode
    }

    pub async fn check(&self, nonce: Option<&str>) -> Result<(), ReplayCheckError> {
        let nonce = match nonce {
            Some(nonce) if !nonce.is_empty() => nonce,
            _ if self.mode.requires_nonce() => return Err(ReplayCheckError::MissingNonce),
            _ => return Ok(()),
        };

        match self.mode {
            ReplayCheckMode::None => Ok(()),
            ReplayCheckMode::NonceLocal => {
                let entry = self.local.entry(nonce.to_string()).or_insert(());
                if entry.is_fresh() {
                    Ok(())
                } else {
                    Err(ReplayCheckError::ReplayDetected)
                }
            }
            ReplayCheckMode::NonceRedis => {
                let Some(redis) = self.redis.as_ref() else {
                    return Err(ReplayCheckError::Backend("redis unavailable".to_string()));
                };
                use fred::interfaces::KeysInterface;
                use fred::types::{Expiration, SetOptions};
                let inserted: Option<String> = redis
                    .next()
                    .set(
                        replay_nonce_key(nonce),
                        "1",
                        Some(Expiration::EX(REPLAY_NONCE_TTL.as_secs() as i64)),
                        Some(SetOptions::NX),
                        false,
                    )
                    .await
                    .map_err(|err| ReplayCheckError::Backend(err.to_string()))?;
                if inserted.is_some() {
                    Ok(())
                } else {
                    Err(ReplayCheckError::ReplayDetected)
                }
            }
        }
    }
}

pub fn replay_nonce_key(nonce: &str) -> String {
    format!("sig_nonce:{nonce}")
}

/// High-level HMAC verification gating on `required` flag, replay window, and algo.
///
/// `received` is the raw signature bytes (the existing [`verify`] takes raw bytes,
/// not base64) — semantically these are the bytes the client sent in the
/// `x-signature` header value. Caller is responsible for any wire-format decoding.
///
/// Replay window is ±60s relative to local clock; future timestamps beyond the
/// window are rejected as well (uses `(now - ts).abs()`).
///
/// Outcome mapping (G7 — T1-D):
/// - `!required` → `NotRequired`
/// - `required` but `secret` empty → `SecretUnavailable` (fail-closed config error)
/// - signature header absent **or** timestamp header absent → `Missing`
/// - timestamp header present but not parseable as `i64` → `Invalid`
/// - timestamp parsed but outside ±60 s → `TimestampDrift`
/// - HMAC compare fails → `Invalid`
/// - HMAC compare passes → `Valid`
pub fn verify_required(
    required: bool,
    algo: SignatureAlgo,
    secret: &[u8],
    received: Option<&[u8]>,
    timestamp_str: Option<&str>,
    input: &SignatureInput<'_>,
) -> SignatureCheckResult {
    if !required {
        return SignatureCheckResult::NotRequired;
    }
    // Fail-closed: an empty HMAC key is accepted by `new_from_slice`, so without
    // this guard any client could forge a valid signature using key="". This is
    // a server misconfiguration (signing required but `signature_secret` NULL),
    // checked before the client-input branches so it cannot be masked by a
    // Missing/Invalid client error.
    if secret.is_empty() {
        return SignatureCheckResult::SecretUnavailable;
    }
    // Header(s) absent — Missing rather than Invalid so the request_filter
    // call site can emit GW_AUTH_SIGNATURE_MISSING.
    let received = match received {
        Some(s) if !s.is_empty() => s,
        _ => return SignatureCheckResult::Missing,
    };
    let raw_ts = match timestamp_str {
        Some(s) if !s.is_empty() => s,
        _ => return SignatureCheckResult::Missing,
    };
    // Header present but malformed → Invalid (the client's input was wrong,
    // not absent).
    let ts = match raw_ts.parse::<i64>() {
        Ok(t) => t,
        Err(_) => return SignatureCheckResult::Invalid,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if (now - ts).abs() > REPLAY_WINDOW_SECS {
        return SignatureCheckResult::TimestampDrift;
    }
    let ok = match algo {
        SignatureAlgo::HmacSha256 => verify(secret, received, input),
        SignatureAlgo::HmacSha512 => verify_sha512(secret, received, input),
    };
    if ok {
        SignatureCheckResult::Valid
    } else {
        SignatureCheckResult::Invalid
    }
}

fn verify_sha512(secret: &[u8], received: &[u8], input: &SignatureInput<'_>) -> bool {
    use sha2::{Digest, Sha512};
    let body_hash = {
        let mut h = Sha512::new();
        h.update(input.body);
        hex::encode(h.finalize())
    };
    let signing = format!(
        "{}\n{}\n{}\n{}",
        input.method, input.path, input.timestamp, body_hash
    );
    let mut mac = match Hmac::<Sha512>::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(signing.as_bytes());
    let expected = mac.finalize().into_bytes();
    expected.ct_eq(received).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input<'a>() -> SignatureInput<'a> {
        SignatureInput {
            method: "POST",
            path: "/v1/messages",
            timestamp: "0",
            body: b"{}",
        }
    }

    // Pins the underlying primitive: HMAC accepts a zero-length key, so an empty
    // secret produces a real, client-computable MAC. This is exactly why the
    // `secret.is_empty()` guard in `verify_required` is required — without it,
    // `verify` returns `true` for a forged empty-key signature.
    #[test]
    fn empty_key_hmac_is_forgeable_at_primitive_layer() {
        let input = sample_input();
        // An attacker who knows method/path/timestamp/body can compute this MAC
        // with key="" — all four are client-controlled.
        let mut mac = Hmac::<Sha256>::new_from_slice(b"")
            .expect("HMAC accepts an empty key (the vulnerability this guards)");
        let body_hash = {
            use sha2::Digest;
            let mut h = Sha256::new();
            h.update(input.body);
            hex::encode(h.finalize())
        };
        mac.update(
            format!(
                "{}\n{}\n{}\n{}",
                input.method, input.path, input.timestamp, body_hash
            )
            .as_bytes(),
        );
        let forged = mac.finalize().into_bytes();
        // Raw `verify` with the empty key WOULD accept the forged MAC.
        assert!(
            verify(b"", &forged, &sample_input()),
            "empty-key HMAC must be forgeable at the primitive layer"
        );
    }

    // The fail-closed guard: required signing with an empty secret never reaches
    // HMAC compare; it returns SecretUnavailable so the caller emits a 500
    // instead of a forgeable Valid/Invalid.
    #[test]
    fn verify_required_rejects_empty_secret_fail_closed() {
        let input = sample_input();
        let r = verify_required(
            true,
            SignatureAlgo::HmacSha256,
            b"", // misconfigured: required but no secret
            Some(b"anything"),
            Some("0"),
            &input,
        );
        assert_eq!(r, SignatureCheckResult::SecretUnavailable);

        // And it wins over a missing client header (config error surfaces first).
        let r2 = verify_required(true, SignatureAlgo::HmacSha256, b"", None, None, &input);
        assert_eq!(r2, SignatureCheckResult::SecretUnavailable);
    }

    #[test]
    fn verify_required_not_required_passes_through() {
        let input = sample_input();
        let r = verify_required(false, SignatureAlgo::HmacSha256, b"", None, None, &input);
        assert_eq!(r, SignatureCheckResult::NotRequired);
    }

    #[tokio::test]
    async fn replay_check_none_does_not_require_nonce() {
        let checker = ReplayChecker::disabled();

        checker.check(None).await.expect("none mode passes");
        checker
            .check(Some("reused"))
            .await
            .expect("first nonce ignored");
        checker
            .check(Some("reused"))
            .await
            .expect("second nonce ignored");
    }

    #[tokio::test]
    async fn replay_check_local_rejects_missing_nonce() {
        let checker = ReplayChecker::new(ReplayCheckMode::NonceLocal, None);

        let err = checker.check(None).await.unwrap_err();
        assert_eq!(err, ReplayCheckError::MissingNonce);
    }

    #[tokio::test]
    async fn replay_check_local_rejects_duplicate_nonce() {
        let checker = ReplayChecker::new(ReplayCheckMode::NonceLocal, None);

        checker.check(Some("n-1")).await.expect("first use wins");
        let err = checker.check(Some("n-1")).await.unwrap_err();
        assert_eq!(err, ReplayCheckError::ReplayDetected);
        checker
            .check(Some("n-2"))
            .await
            .expect("distinct nonce passes");
    }

    #[test]
    fn replay_nonce_key_is_namespaced() {
        assert_eq!(replay_nonce_key("abc"), "sig_nonce:abc");
    }
}
