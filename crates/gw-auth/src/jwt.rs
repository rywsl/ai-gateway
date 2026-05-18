//! JWT verification using a JWKS lookup against the in-memory [`EntityStore`].
//!
//! Algorithm: RS256. The JWT header `kid` is used to look up the PEM-encoded
//! public key in [`EntityStore::jwks`]; if found, the signature & `exp` claim
//! are validated.

use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::{Kid, UserId};
use gw_store::EntityStore;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
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
        .jwks
        .get(&Kid::from(kid))
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
