//! JWT verification.
//!
//! Two surfaces:
//! 1. [`verify_jwt`] async fn — legacy path used by `gw-proxy::filters::request`
//!    when `LlmGateway.jwt_validator` is `None` (no `AuthConfig.global` deployed).
//!    Looks up JWKS via [`EntityStore`] and checks only `exp`. Preserved so
//!    deployments that have not deployed `AuthConfig.global` keep working.
//! 2. [`JwtValidator`] — hybrid surface (T1-A + T1-E). Synchronous secret /
//!    static PEM dispatch layered with an optional async [`JwksResolver`]
//!    (e.g. [`EntityStoreJwksResolver`]) so RS256 + `kid` rotation keeps
//!    working with full `iss`/`aud`/`nbf`/leeway enforcement on top.

use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{Kid, UserId};
use gw_store::EntityStore;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use smallvec::SmallVec;
use std::borrow::Cow;
use std::sync::Arc;

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

// =====================================================================
// T1-A + T1-E hybrid JwtValidator
// =====================================================================
//
// Async-capable validator with iss/aud/nbf/leeway enforcement and an
// optional JWKS resolver for RS256 + `kid` rotation. Errors expose only
// a coarse-grained, token-free reason so logs/metrics cannot leak the
// raw token text.

/// Configuration for [`JwtValidator`].
///
/// At construction time the secret material is optional: HS256 traffic must
/// supply `static_secret_hs256`, RS256 traffic must supply either a
/// `JwksResolver` (attached via [`JwtValidator::attach_jwks`]) or
/// `static_pem_rs256` (or both — the resolver is tried first and the static
/// PEM acts as a fallback when `kid` is missing or the resolver returns
/// `None`).
#[derive(Clone, Default)]
pub struct JwtValidatorConfig {
    pub static_secret_hs256: Option<Vec<u8>>,
    pub static_pem_rs256: Option<Vec<u8>>,
    pub expected_iss: SmallVec<[String; 2]>,
    pub expected_aud: SmallVec<[String; 2]>,
    pub leeway_secs: u32,
    pub require_nbf: bool,
}

/// Async resolver that maps a JWK `kid` to a PEM-encoded public key.
///
/// `EntityStoreJwksResolver` is the production impl (wraps
/// `EntityStore::jwks_or_load`). Tests supply lightweight in-memory impls.
#[async_trait::async_trait]
pub trait JwksResolver: Send + Sync {
    /// Return PEM-encoded public key bytes for `kid`, or `None` if unknown.
    async fn resolve(&self, kid: &str) -> Option<Vec<u8>>;
}

/// `EntityStore`-backed [`JwksResolver`] (production wiring).
///
/// Mirrors the exact JWKS lookup shape of the legacy [`verify_jwt`] fn so
/// behaviour is bit-equivalent for the RS256 + `kid` rotation path.
pub struct EntityStoreJwksResolver {
    pub store: Arc<EntityStore>,
}

#[async_trait::async_trait]
impl JwksResolver for EntityStoreJwksResolver {
    async fn resolve(&self, kid: &str) -> Option<Vec<u8>> {
        self.store
            .jwks_or_load(&Kid::from(kid))
            .await
            .map(|j| j.value.pem.clone().into_bytes())
    }
}

/// Reason for a JWT verification failure. Exposes only a stable enum
/// discriminant + short `reason()` tag — the raw token is never embedded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JwtError {
    /// `iss` claim mismatch.
    InvalidIss,
    /// `aud` claim mismatch.
    InvalidAud,
    /// `nbf` claim in the future (beyond leeway).
    NotYetValid,
    /// `exp` claim in the past (beyond leeway).
    Expired,
    /// Signature/HMAC mismatch, malformed token, missing key, unsupported alg.
    InvalidSignature,
}

impl JwtError {
    /// Stable short tag for metrics/logging. Never contains token bytes.
    pub fn reason(&self) -> &'static str {
        match self {
            JwtError::InvalidIss => "iss",
            JwtError::InvalidAud => "aud",
            JwtError::NotYetValid => "nbf",
            JwtError::Expired => "exp",
            JwtError::InvalidSignature => "sig",
        }
    }
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "jwt invalid: {}", self.reason())
    }
}

impl std::error::Error for JwtError {}

/// Hybrid JWT validator. Construct once with [`JwtValidatorConfig`] and reuse
/// across requests; optionally attach a [`JwksResolver`] for RS256 + `kid`
/// rotation.
pub struct JwtValidator {
    cfg: JwtValidatorConfig,
    jwks: Option<Arc<dyn JwksResolver>>,
}

impl JwtValidator {
    pub fn new(cfg: JwtValidatorConfig) -> Self {
        Self { cfg, jwks: None }
    }

    /// Attach (or replace) the JWKS resolver. Production wires
    /// [`EntityStoreJwksResolver`]; tests use in-memory impls.
    pub fn attach_jwks(&mut self, r: Arc<dyn JwksResolver>) {
        self.jwks = Some(r);
    }

    /// Validate a token and decode the claims to `C`. Picks the verification
    /// key from `cfg` + resolver per the dispatch table:
    ///
    /// | `alg`   | `kid` | resolver wired | key source                                            |
    /// |---------|-------|----------------|-------------------------------------------------------|
    /// | HS256   | -     | -              | `static_secret_hs256` (else `InvalidSignature`)       |
    /// | RS256   | Some  | Some           | `resolver.resolve(kid).await`, fallback `static_pem_rs256` |
    /// | RS256   | None  | -              | `static_pem_rs256` (else `InvalidSignature`)          |
    /// | RS256   | Some  | None           | `static_pem_rs256` (else `InvalidSignature`)          |
    ///
    /// Then runs `jsonwebtoken::decode` with `iss`/`aud`/`exp`/`nbf` according
    /// to the config.
    pub async fn validate<C: DeserializeOwned>(&self, token: &str) -> Result<C, JwtError> {
        let header = decode_header(token).map_err(|_| JwtError::InvalidSignature)?;
        let alg = header.alg;
        if !matches!(alg, Algorithm::HS256 | Algorithm::RS256) {
            return Err(JwtError::InvalidSignature);
        }

        // Key material resolution.
        let key_bytes: Vec<u8> = match alg {
            Algorithm::HS256 => self
                .cfg
                .static_secret_hs256
                .clone()
                .ok_or(JwtError::InvalidSignature)?,
            Algorithm::RS256 => {
                let mut chosen: Option<Vec<u8>> = None;
                if let (Some(kid), Some(jwks)) = (header.kid.as_deref(), self.jwks.as_ref()) {
                    chosen = jwks.resolve(kid).await;
                }
                if chosen.is_none() {
                    chosen = self.cfg.static_pem_rs256.clone();
                }
                chosen.ok_or(JwtError::InvalidSignature)?
            }
            _ => return Err(JwtError::InvalidSignature),
        };

        let decoding_key = match alg {
            Algorithm::HS256 => DecodingKey::from_secret(&key_bytes),
            Algorithm::RS256 => {
                DecodingKey::from_rsa_pem(&key_bytes).map_err(|_| JwtError::InvalidSignature)?
            }
            _ => return Err(JwtError::InvalidSignature),
        };

        let mut val = Validation::new(alg);
        val.leeway = self.cfg.leeway_secs as u64;
        val.validate_exp = true;
        val.validate_nbf = self.cfg.require_nbf;
        if !self.cfg.expected_iss.is_empty() {
            let iss: Vec<String> = self.cfg.expected_iss.iter().cloned().collect();
            val.set_issuer(&iss);
        }
        if !self.cfg.expected_aud.is_empty() {
            let aud: Vec<String> = self.cfg.expected_aud.iter().cloned().collect();
            val.set_audience(&aud);
        } else {
            // `jsonwebtoken` defaults to requiring an audience entry when
            // `validate_aud == true`. We explicitly disable it when the
            // config supplies no expected audience.
            val.validate_aud = false;
        }

        decode::<C>(token, &decoding_key, &val)
            .map(|d| d.claims)
            .map_err(|e| classify_jwt_error(&e))
    }

    /// Convenience: validate and extract the `sub` claim as a [`UserId`].
    /// Used by the proxy `request_filter` JWT branch.
    pub async fn validate_sub(&self, token: &str) -> Result<UserId, JwtError> {
        #[derive(Deserialize)]
        struct OnlySub {
            sub: String,
        }
        let c: OnlySub = self.validate(token).await?;
        Ok(UserId::from(c.sub))
    }
}

fn classify_jwt_error(e: &jsonwebtoken::errors::Error) -> JwtError {
    use jsonwebtoken::errors::ErrorKind::*;
    match e.kind() {
        ExpiredSignature => JwtError::Expired,
        ImmatureSignature => JwtError::NotYetValid,
        InvalidIssuer => JwtError::InvalidIss,
        InvalidAudience => JwtError::InvalidAud,
        _ => JwtError::InvalidSignature,
    }
}
