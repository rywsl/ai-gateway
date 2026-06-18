//! T1-E — hybrid `JwtValidator`. Covers HS256 static-secret happy path, the
//! `iss`/`aud`/`nbf`/`exp` rejection codes (mapped to the new `JwtError` enum),
//! and the RS256 JWKS-resolver dispatch (kid lookup + static_pem_rs256
//! fallback). The RS256 keypair is baked into the test as a 2048-bit
//! `openssl genrsa` fixture so the test stays hermetic (no `rsa` crate dep).

use gw_auth::jwt::{JwksResolver, JwtError, JwtValidator, JwtValidatorConfig};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use smallvec::smallvec;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// --- Fixtures ----------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    iss: Option<String>,
    aud: Option<String>,
    exp: usize,
    nbf: Option<usize>,
}

fn now_unix() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize
}

fn make_hs256_token(secret: &[u8], c: &Claims) -> String {
    encode(&Header::default(), c, &EncodingKey::from_secret(secret)).unwrap()
}

fn make_rs256_token(pem_priv: &[u8], kid: Option<&str>, c: &Claims) -> String {
    let mut hdr = Header::new(jsonwebtoken::Algorithm::RS256);
    if let Some(k) = kid {
        hdr.kid = Some(k.to_string());
    }
    encode(&hdr, c, &EncodingKey::from_rsa_pem(pem_priv).unwrap()).unwrap()
}

// 2048-bit RSA keypair baked-in for hermetic tests.
const TEST_RSA_PRIVATE_PEM: &[u8] = b"-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEAoKdRhJDtz1SI50K6HWsWXSh0+xscdcqbNRlZ7JQqgroIvkDE
KJ31F5TFrYV20+6N3SSFeyb5aUJJRjwkArTPhj8IAQi76485l0+sKnz8NIlyew4q
udr3vEjek41x090jsPJui/25FIepLdRaMRcZGcDO8C4FZTdSX3OPv30KtlLMX0a9
dfcnvzUxa6uuEjNb/LRg6fS37+uETgZ/Wv6zOfZPiRobw8UsWqIUw54qDhdBNTX6
h9rNnKfv+1HpFjn1MoolvdInKKtbtM/Fj2+jLCpBgVMdJ9iRCu56APwpAI/j1QJx
+Ana9hi8HDCetB2uIMmvyeDNd/KCrKt2ybmJEQIDAQABAoIBABF3P1iPPRt+UEf8
LCnIm6zTGiy2nNh9h3Q1Zl5fGWQO/6ZXejwEOEIxDXC5Ats1ozy4nL5v1UvJRhgZ
r7yPlcQ9lAwwu+V3M8ashcY0wmKf0s9oEeMU2DwjG/ekuAxVjWzOXpBfrEB8Izt1
S7zsivEO0jCwgCup4Q25jZY+7Ow3Vhg7+UNWFyoKh+amBWMHFeYYMJ7lB68PPa4V
PXmsOhVn6zh2e+98VrF/EtStBX1VRxPxTcmtzFX8SSaB60uTCtOIPe6pugK+l6bO
0s4g+PsmebR1Ks8HxWX/3wgjPkIl2fSNxWXvFooZXk+fLmOn0v8xZUh9swt7/2O/
fQLi3LUCgYEA03tEdkdzkFE/BvCabSq5slzND9Zol7tPSLExmDbN2vy5h+yx6QXE
3Rs1+qRL5yKqja7S/TiInOwyk0BOSntazC1w25pgco7JnO7D0bwAWs5PUHq1hbw4
ftABX73I+dQZIwjf0l27Mkrh/kb+6hDVn1Avhi1C0f4J7HQlLGIuNVcCgYEAwnjv
gTuwBMuR2hlptiQ3Me+Xvgxo7gIhr4K2qnWCvM1Q4L+FZRS52EEMKFwMTNLSMDGK
6s592Cp9XqRpanP7yCscLug1SIHb6Qxzu09dWYliwu4m59w1YVHpXWEMONxWIAhm
Pr8eLCpNCgpREmuniTDzrrxhNKt9fAl/L2hgC9cCgYAnSm+dygied3nOQEVwos99
Y+B1YNBt4E8KomEkdooodWvC5SbotDmvGq8csgmoF7i4OhLZGPfIr9l1oizpiaGE
7axaIIRqxDyQ+4tGHDe0pz/1I9c10ZJtfRfEQe5k/BLvf2aWHvP4lBEIvQf/VMFb
p/osY5xWh2wwkiOV62fKYQKBgQCuniSAJ/YO+DMIYNU4xSACaJ4Z+c6mF6fjjj0y
pb00hi7e/1lpv8h300B1ZOFugcZVm8aeu7bnyI810e8NtQMZYww1Wi+t4Vxyz8mD
IOozIVdarLimbGZJ8mDWhfORgtaagLA+GyGeioUOROEy5xNR7nCAL4og3vMX0vcu
vEuxlQKBgEKSeI2VOMy/GzqmzzScZljrh01z5MNyzjJ0lMhmvuf7xTW7IiPNuiQJ
+ZZrekbwiOq3qQt9z4+OVuT46Gf5wRGD/Xg4F7mKa2dSz+wY4PyArk55AV1wrfpI
BK0UQdgxOn5AiK/phOvM4VhbOxZL2dkL7P7Mh8c/zmXIN5laAA//
-----END RSA PRIVATE KEY-----";

const TEST_RSA_PUBLIC_PEM: &[u8] = b"-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAoKdRhJDtz1SI50K6HWsW
XSh0+xscdcqbNRlZ7JQqgroIvkDEKJ31F5TFrYV20+6N3SSFeyb5aUJJRjwkArTP
hj8IAQi76485l0+sKnz8NIlyew4qudr3vEjek41x090jsPJui/25FIepLdRaMRcZ
GcDO8C4FZTdSX3OPv30KtlLMX0a9dfcnvzUxa6uuEjNb/LRg6fS37+uETgZ/Wv6z
OfZPiRobw8UsWqIUw54qDhdBNTX6h9rNnKfv+1HpFjn1MoolvdInKKtbtM/Fj2+j
LCpBgVMdJ9iRCu56APwpAI/j1QJx+Ana9hi8HDCetB2uIMmvyeDNd/KCrKt2ybmJ
EQIDAQAB
-----END PUBLIC KEY-----";

// --- Fake JwksResolver -------------------------------------------------

struct CountingJwks {
    kid_match: Option<String>,
    pem: Vec<u8>,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl JwksResolver for CountingJwks {
    async fn resolve(&self, kid: &str) -> Option<Vec<u8>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.kid_match {
            Some(k) if k == kid => Some(self.pem.clone()),
            _ => None,
        }
    }
}

// --- HS256 path --------------------------------------------------------

#[tokio::test]
async fn hs256_static_secret_happy() {
    let secret = b"top-secret";
    let token = make_hs256_token(
        secret,
        &Claims {
            sub: "u1".into(),
            iss: Some("my-iss".into()),
            aud: Some("my-aud".into()),
            exp: now_unix() + 60,
            nbf: Some(now_unix() - 5),
        },
    );
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: Some(secret.to_vec()),
        static_pem_rs256: None,
        expected_iss: smallvec!["my-iss".into()],
        expected_aud: smallvec!["my-aud".into()],
        leeway_secs: 5,
        require_nbf: true,
        ..JwtValidatorConfig::default()
    });
    let c: Claims = v.validate(&token).await.unwrap();
    assert_eq!(c.sub, "u1");
}

#[tokio::test]
async fn iss_mismatch_rejects() {
    let secret = b"top-secret";
    let token = make_hs256_token(
        secret,
        &Claims {
            sub: "u1".into(),
            iss: Some("WRONG".into()),
            aud: Some("my-aud".into()),
            exp: now_unix() + 60,
            nbf: None,
        },
    );
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: Some(secret.to_vec()),
        static_pem_rs256: None,
        expected_iss: smallvec!["my-iss".into()],
        expected_aud: smallvec!["my-aud".into()],
        leeway_secs: 5,
        require_nbf: false,
        ..JwtValidatorConfig::default()
    });
    assert!(matches!(
        v.validate::<Claims>(&token).await,
        Err(JwtError::InvalidIss)
    ));
}

#[tokio::test]
async fn aud_mismatch_rejects() {
    let secret = b"top-secret";
    let token = make_hs256_token(
        secret,
        &Claims {
            sub: "u1".into(),
            iss: Some("my-iss".into()),
            aud: Some("WRONG".into()),
            exp: now_unix() + 60,
            nbf: None,
        },
    );
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: Some(secret.to_vec()),
        static_pem_rs256: None,
        expected_iss: smallvec!["my-iss".into()],
        expected_aud: smallvec!["my-aud".into()],
        leeway_secs: 5,
        require_nbf: false,
        ..JwtValidatorConfig::default()
    });
    assert!(matches!(
        v.validate::<Claims>(&token).await,
        Err(JwtError::InvalidAud)
    ));
}

#[tokio::test]
async fn nbf_future_rejects() {
    let secret = b"top-secret";
    let token = make_hs256_token(
        secret,
        &Claims {
            sub: "u1".into(),
            iss: Some("my-iss".into()),
            aud: Some("my-aud".into()),
            exp: now_unix() + 600,
            nbf: Some(now_unix() + 120),
        },
    );
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: Some(secret.to_vec()),
        static_pem_rs256: None,
        expected_iss: smallvec!["my-iss".into()],
        expected_aud: smallvec!["my-aud".into()],
        leeway_secs: 0,
        require_nbf: true,
        ..JwtValidatorConfig::default()
    });
    assert!(matches!(
        v.validate::<Claims>(&token).await,
        Err(JwtError::NotYetValid)
    ));
}

#[tokio::test]
async fn nbf_within_leeway_passes() {
    let secret = b"top-secret";
    let token = make_hs256_token(
        secret,
        &Claims {
            sub: "u1".into(),
            iss: Some("my-iss".into()),
            aud: Some("my-aud".into()),
            exp: now_unix() + 600,
            nbf: Some(now_unix() + 3),
        },
    );
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: Some(secret.to_vec()),
        static_pem_rs256: None,
        expected_iss: smallvec!["my-iss".into()],
        expected_aud: smallvec!["my-aud".into()],
        leeway_secs: 30,
        require_nbf: true,
        ..JwtValidatorConfig::default()
    });
    let c: Claims = v.validate(&token).await.unwrap();
    assert_eq!(c.sub, "u1");
}

#[tokio::test]
async fn expired_rejects() {
    let secret = b"top-secret";
    let token = make_hs256_token(
        secret,
        &Claims {
            sub: "u1".into(),
            iss: Some("my-iss".into()),
            aud: Some("my-aud".into()),
            exp: now_unix() - 120,
            nbf: None,
        },
    );
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: Some(secret.to_vec()),
        static_pem_rs256: None,
        expected_iss: smallvec!["my-iss".into()],
        expected_aud: smallvec!["my-aud".into()],
        leeway_secs: 5,
        require_nbf: false,
        ..JwtValidatorConfig::default()
    });
    assert!(matches!(
        v.validate::<Claims>(&token).await,
        Err(JwtError::Expired)
    ));
}

#[tokio::test]
async fn hs256_no_static_secret_rejects() {
    let secret = b"top-secret";
    let token = make_hs256_token(
        secret,
        &Claims {
            sub: "u1".into(),
            iss: None,
            aud: None,
            exp: now_unix() + 60,
            nbf: None,
        },
    );
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: None,
        static_pem_rs256: None,
        expected_iss: smallvec![],
        expected_aud: smallvec![],
        leeway_secs: 0,
        require_nbf: false,
        ..JwtValidatorConfig::default()
    });
    assert!(matches!(
        v.validate::<Claims>(&token).await,
        Err(JwtError::InvalidSignature)
    ));
}

// --- RS256 / JWKS path ------------------------------------------------

#[tokio::test]
async fn rs256_jwks_kid_lookup_happy() {
    let resolver = Arc::new(CountingJwks {
        kid_match: Some("k1".into()),
        pem: TEST_RSA_PUBLIC_PEM.to_vec(),
        calls: AtomicUsize::new(0),
    });
    let mut v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: None,
        static_pem_rs256: None,
        expected_iss: smallvec!["my-iss".into()],
        expected_aud: smallvec!["my-aud".into()],
        leeway_secs: 5,
        require_nbf: false,
        ..JwtValidatorConfig::default()
    });
    v.attach_jwks(resolver.clone());
    let token = make_rs256_token(
        TEST_RSA_PRIVATE_PEM,
        Some("k1"),
        &Claims {
            sub: "u1".into(),
            iss: Some("my-iss".into()),
            aud: Some("my-aud".into()),
            exp: now_unix() + 60,
            nbf: None,
        },
    );
    let c: Claims = v.validate(&token).await.unwrap();
    assert_eq!(c.sub, "u1");
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn jwks_resolver_miss_rejects_when_static_fallback_disabled() {
    let resolver = Arc::new(CountingJwks {
        kid_match: Some("different".into()),
        pem: vec![],
        calls: AtomicUsize::new(0),
    });
    let mut v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: None,
        static_pem_rs256: Some(TEST_RSA_PUBLIC_PEM.to_vec()),
        allow_static_pem_fallback_for_unknown_kid: false,
        expected_iss: smallvec![],
        expected_aud: smallvec![],
        leeway_secs: 5,
        require_nbf: false,
        ..JwtValidatorConfig::default()
    });
    v.attach_jwks(resolver.clone());
    let token = make_rs256_token(
        TEST_RSA_PRIVATE_PEM,
        Some("unknown"),
        &Claims {
            sub: "u1".into(),
            iss: None,
            aud: None,
            exp: now_unix() + 60,
            nbf: None,
        },
    );
    assert!(matches!(
        v.validate::<Claims>(&token).await,
        Err(JwtError::InvalidSignature)
    ));
    assert_eq!(
        resolver.calls.load(Ordering::SeqCst),
        1,
        "resolver was called before unknown-kid rejection"
    );
}

#[tokio::test]
async fn jwks_resolver_miss_falls_back_to_static_pem_when_explicitly_enabled() {
    let resolver = Arc::new(CountingJwks {
        kid_match: Some("different".into()),
        pem: vec![],
        calls: AtomicUsize::new(0),
    });
    let mut v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: None,
        static_pem_rs256: Some(TEST_RSA_PUBLIC_PEM.to_vec()),
        allow_static_pem_fallback_for_unknown_kid: true,
        expected_iss: smallvec![],
        expected_aud: smallvec![],
        leeway_secs: 5,
        require_nbf: false,
        allowed_algs: smallvec![jsonwebtoken::Algorithm::RS256],
        ..JwtValidatorConfig::default()
    });
    v.attach_jwks(resolver.clone());
    let token = make_rs256_token(
        TEST_RSA_PRIVATE_PEM,
        Some("unknown"),
        &Claims {
            sub: "u1".into(),
            iss: None,
            aud: None,
            exp: now_unix() + 60,
            nbf: None,
        },
    );
    let c: Claims = v.validate(&token).await.unwrap();
    assert_eq!(c.sub, "u1");
    assert_eq!(
        resolver.calls.load(Ordering::SeqCst),
        1,
        "resolver was called before fallback"
    );
}

#[tokio::test]
async fn allowed_algs_rejects_hs256_when_only_rs256_is_configured() {
    let secret = b"top-secret";
    let token = make_hs256_token(
        secret,
        &Claims {
            sub: "u1".into(),
            iss: None,
            aud: None,
            exp: now_unix() + 60,
            nbf: None,
        },
    );
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: Some(secret.to_vec()),
        static_pem_rs256: Some(TEST_RSA_PUBLIC_PEM.to_vec()),
        allow_static_pem_fallback_for_unknown_kid: false,
        expected_iss: smallvec![],
        expected_aud: smallvec![],
        leeway_secs: 0,
        require_nbf: false,
        allowed_algs: smallvec![jsonwebtoken::Algorithm::RS256],
        ..JwtValidatorConfig::default()
    });

    assert!(matches!(
        v.validate::<Claims>(&token).await,
        Err(JwtError::InvalidSignature)
    ));
}

#[tokio::test]
async fn rs256_no_jwks_no_static_pem_rejects() {
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: None,
        static_pem_rs256: None,
        expected_iss: smallvec![],
        expected_aud: smallvec![],
        leeway_secs: 5,
        require_nbf: false,
        ..JwtValidatorConfig::default()
    });
    let token = make_rs256_token(
        TEST_RSA_PRIVATE_PEM,
        Some("k1"),
        &Claims {
            sub: "u1".into(),
            iss: None,
            aud: None,
            exp: now_unix() + 60,
            nbf: None,
        },
    );
    assert!(matches!(
        v.validate::<Claims>(&token).await,
        Err(JwtError::InvalidSignature)
    ));
}

#[tokio::test]
async fn rs256_no_kid_uses_static_pem() {
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: None,
        static_pem_rs256: Some(TEST_RSA_PUBLIC_PEM.to_vec()),
        expected_iss: smallvec![],
        expected_aud: smallvec![],
        leeway_secs: 5,
        require_nbf: false,
        ..JwtValidatorConfig::default()
    });
    let token = make_rs256_token(
        TEST_RSA_PRIVATE_PEM,
        None,
        &Claims {
            sub: "u1".into(),
            iss: None,
            aud: None,
            exp: now_unix() + 60,
            nbf: None,
        },
    );
    let c: Claims = v.validate(&token).await.unwrap();
    assert_eq!(c.sub, "u1");
}

#[tokio::test]
async fn reason_does_not_leak_token() {
    let token = "not.a.real.jwt";
    let v = JwtValidator::new(JwtValidatorConfig {
        static_secret_hs256: Some(b"x".to_vec()),
        static_pem_rs256: None,
        expected_iss: smallvec![],
        expected_aud: smallvec![],
        leeway_secs: 0,
        require_nbf: false,
        ..JwtValidatorConfig::default()
    });
    let err = v.validate::<Claims>(token).await.unwrap_err();
    assert_eq!(err.reason(), "sig");
    let dbg = format!("{err:?}");
    let disp = format!("{err}");
    assert!(!dbg.contains(token));
    assert!(!disp.contains(token));
}
