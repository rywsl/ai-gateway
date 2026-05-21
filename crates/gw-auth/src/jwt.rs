//! JWT verification.
//!
//! Two surfaces:
//! 1. [`verify_jwt`] async fn — legacy path used by `gw-proxy::filters::request`,
//!    looks up JWKS via [`EntityStore`]. Kept until Wave 6 wires `JwtValidator` in.
//! 2. [`JwtValidator`] — new sync surface (T1-A) with full `iss`/`aud`/`nbf`/leeway
//!    validation. Independent of `EntityStore`; caller supplies secret bytes
//!    or PEM directly.

use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{Kid, UserId};
use gw_store::EntityStore;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use smallvec::SmallVec;
use std::borrow::Cow;

#[derive(Deserialize)]
struct Claims {
    sub: String,
    #[allow(dead_code)]
    exp: usize,
}

pub async fn verify_jwt(token: &str, store: &EntityStore) -> Result<UserId, ErrorIR<'static>> {
    let hdr = decode_header(token).map_err(|_| auth_err())?;
    let kid = hdr.kid.ok_or_else(auth_err)?;
    let jwk = store
        .jwks_or_load(&Kid::from(kid))
        .await
        .ok_or_else(auth_err)?;
    let key = DecodingKey::from_rsa_pem(jwk.value.pem.as_bytes()).map_err(|_| auth_err())?;
    let mut val = Validation::new(Algorithm::RS256);
    val.validate_exp = true;
    let data = decode::<Claims>(token, &key, &val).map_err(|_| auth_err())?;
    Ok(UserId::from(data.claims.sub))
}

fn auth_err() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientAuth,
        status: 401,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_AUTH_INVALID_JWT),
        message: Cow::Borrowed("auth failed"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}

// === T1-A new surface ===
//
// Synchronous, EntityStore-independent JWT validator with full iss/aud/nbf
// support. Errors expose only a coarse-grained `reason()` tag so logs and
// metrics can never accidentally include the token text itself.

/// Synchronous JWT validator. Construct once with [`JwtValidatorConfig`] and
/// reuse across requests.
pub struct JwtValidator {
    cfg: JwtValidatorConfig,
}

/// Configuration for [`JwtValidator`].
///
/// `secret` carries either an HS256 shared secret (raw bytes) or an RS256
/// public key in PEM form; the decoder picks based on the token header `alg`.
#[derive(Clone)]
pub struct JwtValidatorConfig {
    pub secret: Vec<u8>,
    pub expected_iss: SmallVec<[String; 2]>,
    pub expected_aud: SmallVec<[String; 2]>,
    pub leeway_secs: u32,
    pub require_nbf: bool,
}

/// Reason for a JWT verification failure. Only the static tag is exposed via
/// [`JwtError::reason`]; the token itself is never embedded.
#[derive(Debug)]
pub struct JwtError {
    reason: &'static str,
}

impl JwtError {
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "jwt invalid: {}", self.reason)
    }
}

impl std::error::Error for JwtError {}

impl JwtValidator {
    pub fn new(cfg: JwtValidatorConfig) -> Self {
        Self { cfg }
    }

    pub fn verify<C: DeserializeOwned>(&self, token: &str) -> Result<C, JwtError> {
        let header = decode_header(token).map_err(|_| JwtError { reason: "header" })?;
        let alg = header.alg;
        if !matches!(alg, Algorithm::HS256 | Algorithm::RS256) {
            return Err(JwtError { reason: "alg" });
        }
        let mut val = Validation::new(alg);
        val.leeway = self.cfg.leeway_secs as u64;
        val.validate_exp = true;
        val.validate_nbf = self.cfg.require_nbf;
        let iss: Vec<String> = self.cfg.expected_iss.iter().cloned().collect();
        val.set_issuer(&iss);
        let aud: Vec<String> = self.cfg.expected_aud.iter().cloned().collect();
        val.set_audience(&aud);

        let key = match alg {
            Algorithm::HS256 => DecodingKey::from_secret(&self.cfg.secret),
            Algorithm::RS256 => DecodingKey::from_rsa_pem(&self.cfg.secret)
                .map_err(|_| JwtError { reason: "key" })?,
            _ => return Err(JwtError { reason: "alg" }),
        };

        match decode::<C>(token, &key, &val) {
            Ok(d) => Ok(d.claims),
            Err(e) => Err(JwtError {
                reason: classify_jwt_error(&e),
            }),
        }
    }
}

fn classify_jwt_error(e: &jsonwebtoken::errors::Error) -> &'static str {
    use jsonwebtoken::errors::ErrorKind::*;
    match e.kind() {
        InvalidSignature => "sig",
        ExpiredSignature => "exp",
        ImmatureSignature => "nbf",
        InvalidIssuer => "iss",
        InvalidAudience => "aud",
        InvalidToken | Json(_) | Base64(_) => "format",
        InvalidAlgorithm | InvalidAlgorithmName => "alg",
        InvalidKeyFormat | InvalidRsaKey(_) | InvalidEcdsaKey => "key",
        MissingRequiredClaim(_) => "missing",
        _ => "other",
    }
}
