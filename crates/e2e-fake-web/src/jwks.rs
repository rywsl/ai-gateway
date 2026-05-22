//! JWKS rotation endpoint.
//!
//! `POST /jwks/rotate {kid}` generates a fresh RS256 keypair, persists
//! the SPKI-PEM **public** key in the `jwks` entity (which emits an
//! `events:entity ns=jwks` invalidation so the gateway picks it up
//! mid-traffic), and returns the PKCS#8-PEM **private** key in the
//! response so pytest can sign JWTs with it.
//!
//! ## Contract with the gateway
//!
//! The gateway's `jwks` payload deserializes into `JwksEntry { kid,
//! pem }` and feeds `pem` to `DecodingKey::from_rsa_pem`. That helper
//! accepts the SPKI form (`-----BEGIN PUBLIC KEY-----`), which is what
//! `RsaPublicKey::to_public_key_pem` (re-exported via
//! `rsa::pkcs8::EncodePublicKey`) produces.

use crate::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::{RsaPrivateKey, RsaPublicKey};
use serde::{Deserialize, Serialize};

const RSA_BITS: usize = 2048;

#[derive(Deserialize)]
pub struct RotateRequest {
    pub kid: String,
}

#[derive(Serialize)]
pub struct RotateResponse {
    pub private_pem: String,
}

/// Generate the keypair on a blocking thread. `rand::thread_rng()` is
/// `!Send` AND RSA keygen at 2048 bits is CPU-bound, so we offload it
/// to `spawn_blocking` — this also keeps the outer handler future
/// `Send`, which axum requires.
fn generate_keypair() -> Result<(String, String), String> {
    let mut rng = rand::thread_rng();
    let private_key =
        RsaPrivateKey::new(&mut rng, RSA_BITS).map_err(|e| format!("RsaPrivateKey::new: {e}"))?;
    let public_key = RsaPublicKey::from(&private_key);
    let private_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| format!("to_pkcs8_pem: {e}"))?
        .to_string();
    let public_pem = public_key
        .to_public_key_pem(LineEnding::LF)
        .map_err(|e| format!("to_public_key_pem: {e}"))?;
    Ok((private_pem, public_pem))
}

pub async fn rotate(
    State(state): State<AppState>,
    Json(req): Json<RotateRequest>,
) -> impl IntoResponse {
    let (private_pem, public_pem) = match tokio::task::spawn_blocking(generate_keypair).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(msg)) => return (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
        Err(join_err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("keygen task panicked: {join_err}"),
            )
                .into_response()
        }
    };

    // Sanity: gateway expects SPKI public PEM. If somehow the encoder
    // emitted PKCS#1 (`BEGIN RSA PUBLIC KEY`), fail loudly rather than
    // ship a key the gateway will reject at JWT verify time with a
    // non-obvious error.
    if !public_pem.starts_with("-----BEGIN PUBLIC KEY-----") {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "public PEM not SPKI (got prefix: {:?})",
                &public_pem[..public_pem.len().min(40)]
            ),
        )
            .into_response();
    }

    let payload = serde_json::json!({
        "kid": req.kid,
        "pem": public_pem,
    });
    if let Err(e) = state.store.put_entity("jwks", &req.kid, payload).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    Json(RotateResponse { private_pem }).into_response()
}
