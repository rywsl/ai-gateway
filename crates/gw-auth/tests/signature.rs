//! Tests for [`gw_auth::signature`].

use gw_auth::signature::*;
use gw_core::entity::SignatureAlgo;

fn sign_sha256(secret: &[u8], method: &str, path: &str, ts: &str, body: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    let body_hash = {
        let mut h = Sha256::new();
        h.update(body);
        hex::encode(h.finalize())
    };
    let signing = format!("{method}\n{path}\n{ts}\n{body_hash}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[test]
fn signature_match() {
    let secret = b"my-secret";
    let body = br#"{"x":1}"#;
    let input = SignatureInput {
        method: "POST",
        path: "/v1/chat",
        timestamp: "1716000000",
        body,
    };
    let body_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(body);
        hex::encode(h.finalize())
    };
    let signing = format!("POST\n/v1/chat\n1716000000\n{body_hash}");
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing.as_bytes());
    let expected = mac.finalize().into_bytes();
    assert!(verify(secret, &expected, &input));
}

#[test]
fn signature_mismatch_with_tampered_body() {
    let secret = b"s";
    let input2 = SignatureInput {
        method: "POST",
        path: "/x",
        timestamp: "1",
        body: b"b",
    };
    // Compute signature over body "a", verify against input claiming body "b".
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    let body_hash = {
        let mut h = Sha256::new();
        h.update(b"a");
        hex::encode(h.finalize())
    };
    let s = format!("POST\n/x\n1\n{body_hash}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(s.as_bytes());
    let sig = mac.finalize().into_bytes();
    assert!(!verify(secret, &sig, &input2));
}

// ---------------------------------------------------------------------------
// verify_required — five-variant outcome mapping (T1-D / G7).
// ---------------------------------------------------------------------------

#[test]
fn verify_required_off_returns_not_required() {
    let input = SignatureInput {
        method: "POST",
        path: "/x",
        timestamp: "",
        body: b"",
    };
    let r = verify_required(false, SignatureAlgo::HmacSha256, b"", None, None, &input);
    assert_eq!(r, SignatureCheckResult::NotRequired);
}

#[test]
fn verify_required_missing_sig_header_returns_missing() {
    let ts = now_secs().to_string();
    let input = SignatureInput {
        method: "POST",
        path: "/x",
        timestamp: &ts,
        body: b"body",
    };
    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        b"secret",
        None,
        Some(&ts),
        &input,
    );
    assert_eq!(r, SignatureCheckResult::Missing);
}

#[test]
fn verify_required_missing_timestamp_returns_missing() {
    let input = SignatureInput {
        method: "POST",
        path: "/x",
        timestamp: "",
        body: b"body",
    };
    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        b"secret",
        Some(b"deadbeef"),
        None,
        &input,
    );
    assert_eq!(r, SignatureCheckResult::Missing);
}

#[test]
fn verify_required_drifted_timestamp_returns_timestamp_drift() {
    let secret = b"secret";
    let stale = (now_secs() - 400).to_string();
    let body = b"body";
    let sig = sign_sha256(secret, "POST", "/x", &stale, body);
    let input = SignatureInput {
        method: "POST",
        path: "/x",
        timestamp: &stale,
        body,
    };
    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        secret,
        Some(&sig),
        Some(&stale),
        &input,
    );
    assert_eq!(r, SignatureCheckResult::TimestampDrift);
}

#[test]
fn verify_required_malformed_timestamp_returns_invalid() {
    let input = SignatureInput {
        method: "POST",
        path: "/x",
        timestamp: "not-a-number",
        body: b"body",
    };
    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        b"secret",
        Some(b"deadbeef"),
        Some("not-a-number"),
        &input,
    );
    assert_eq!(r, SignatureCheckResult::Invalid);
}

#[test]
fn verify_required_valid_sig_returns_valid() {
    let secret = b"secret";
    let ts = now_secs().to_string();
    let body = b"body";
    let sig = sign_sha256(secret, "POST", "/x", &ts, body);
    let input = SignatureInput {
        method: "POST",
        path: "/x",
        timestamp: &ts,
        body,
    };
    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        secret,
        Some(&sig),
        Some(&ts),
        &input,
    );
    assert_eq!(r, SignatureCheckResult::Valid);
}

#[test]
fn verify_required_bad_hmac_returns_invalid() {
    let ts = now_secs().to_string();
    let input = SignatureInput {
        method: "POST",
        path: "/x",
        timestamp: &ts,
        body: b"body",
    };
    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        b"secret",
        Some(b"deadbeef"),
        Some(&ts),
        &input,
    );
    assert_eq!(r, SignatureCheckResult::Invalid);
}
