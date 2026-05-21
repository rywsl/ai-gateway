use gw_auth::jwt::{JwtValidator, JwtValidatorConfig};
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

#[test]
fn rejects_iss_mismatch() {
    let secret = b"secret";
    let token = make_token(secret, "evil", "gw", 60, -1);
    let v = JwtValidator::new(JwtValidatorConfig {
        secret: secret.to_vec(),
        expected_iss: smallvec!["good".into()],
        expected_aud: smallvec!["gw".into()],
        leeway_secs: 30,
        require_nbf: true,
    });
    let err = v.verify::<Claims>(&token).unwrap_err();
    assert_eq!(err.reason(), "iss");
}

#[test]
fn rejects_aud_mismatch() {
    let secret = b"secret";
    let token = make_token(secret, "good", "evil", 60, -1);
    let v = JwtValidator::new(JwtValidatorConfig {
        secret: secret.to_vec(),
        expected_iss: smallvec!["good".into()],
        expected_aud: smallvec!["gw".into()],
        leeway_secs: 30,
        require_nbf: true,
    });
    let err = v.verify::<Claims>(&token).unwrap_err();
    assert_eq!(err.reason(), "aud");
}

#[test]
fn rejects_nbf_in_future() {
    let secret = b"secret";
    let token = make_token(secret, "good", "gw", 600, 120);
    let v = JwtValidator::new(JwtValidatorConfig {
        secret: secret.to_vec(),
        expected_iss: smallvec!["good".into()],
        expected_aud: smallvec!["gw".into()],
        leeway_secs: 0,
        require_nbf: true,
    });
    let err = v.verify::<Claims>(&token).unwrap_err();
    assert_eq!(err.reason(), "nbf");
}

#[test]
fn accepts_valid_with_leeway() {
    let secret = b"secret";
    let token = make_token(secret, "good", "gw", -10, -100);
    let v = JwtValidator::new(JwtValidatorConfig {
        secret: secret.to_vec(),
        expected_iss: smallvec!["good".into()],
        expected_aud: smallvec!["gw".into()],
        leeway_secs: 30,
        require_nbf: true,
    });
    let claims = v.verify::<Claims>(&token).unwrap();
    assert_eq!(claims.sub, "u1");
}

#[test]
fn reason_does_not_contain_token() {
    let v = JwtValidator::new(JwtValidatorConfig {
        secret: b"secret".to_vec(),
        expected_iss: smallvec!["a".into()],
        expected_aud: smallvec!["b".into()],
        leeway_secs: 0,
        require_nbf: false,
    });
    let secret_token = "not.a.real.jwt";
    let err = v.verify::<serde_json::Value>(secret_token).unwrap_err();
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
