//! Tests for [`gw_auth::signature`].

use gw_auth::signature::*;

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
