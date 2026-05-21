//! Sibling to `jwt_validator_hybrid.rs` — exercises the synchronous-feel
//! `JwtValidator::validate` path with HS256 static secrets. Updated for the
//! T1-E enum-shaped `JwtError`.

use gw_auth::jwt::{JwtError, JwtValidator, JwtValidatorConfig};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use smallvec::smallvec;

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    iss: String,
    aud: String,
    exp: usize,
    nbf: usize,
}

fn now() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize
}

fn make_token(secret: &[u8], iss: &str, aud: &str, exp_offset: i64, nbf_offset: i64) -> String {
    let claims = Claims {
        sub: "u1".into(),
        iss: iss.into(),
        aud: aud.into(),
        exp: (now() as i64 + exp_offset).max(0) as usize,
        nbf: (now() as i64 + nbf_offset).max(0) as usize,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .unwrap()
}

fn cfg(secret: &[u8], iss: &str, aud: &str, leeway: u32, require_nbf: bool) -> JwtValidatorConfig {
    JwtValidatorConfig {
        static_secret_hs256: Some(secret.to_vec()),
        static_pem_rs256: None,
        expected_iss: smallvec![iss.into()],
        expected_aud: smallvec![aud.into()],
        leeway_secs: leeway,
        require_nbf,
    }
}

#[tokio::test]
async fn rejects_iss_mismatch() {
    let secret = b"secret";
    let token = make_token(secret, "evil", "gw", 60, -1);
    let v = JwtValidator::new(cfg(secret, "good", "gw", 30, true));
    let err = v.validate::<Claims>(&token).await.unwrap_err();
    assert_eq!(err, JwtError::InvalidIss);
    assert_eq!(err.reason(), "iss");
}

#[tokio::test]
async fn rejects_aud_mismatch() {
    let secret = b"secret";
    let token = make_token(secret, "good", "evil", 60, -1);
    let v = JwtValidator::new(cfg(secret, "good", "gw", 30, true));
    let err = v.validate::<Claims>(&token).await.unwrap_err();
    assert_eq!(err, JwtError::InvalidAud);
}

#[tokio::test]
async fn rejects_nbf_in_future() {
    let secret = b"secret";
    let token = make_token(secret, "good", "gw", 600, 120);
    let v = JwtValidator::new(cfg(secret, "good", "gw", 0, true));
    let err = v.validate::<Claims>(&token).await.unwrap_err();
    assert_eq!(err, JwtError::NotYetValid);
}

#[tokio::test]
async fn accepts_valid_with_leeway() {
    let secret = b"secret";
    let token = make_token(secret, "good", "gw", -10, -100);
    let v = JwtValidator::new(cfg(secret, "good", "gw", 30, true));
    let claims = v.validate::<Claims>(&token).await.unwrap();
    assert_eq!(claims.sub, "u1");
}

#[tokio::test]
async fn reason_does_not_contain_token() {
    let v = JwtValidator::new(cfg(b"secret", "a", "b", 0, false));
    let secret_token = "not.a.real.jwt";
    let err = v.validate::<serde_json::Value>(secret_token).await.unwrap_err();
    let dbg = format!("{err:?}");
    let disp = format!("{err}");
    assert!(
        !dbg.contains(secret_token),
        "reason format leaks token: {dbg}"
    );
    assert!(
        !disp.contains(secret_token),
        "display leaks token: {disp}"
    );
}
