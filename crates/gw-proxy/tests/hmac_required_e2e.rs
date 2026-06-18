//! Body-after HMAC verification helper.
//!
//! Real e2e (with full Pingora session + wiremock) lives in
//! e2e_failure_modes.rs once T6-C wiring lands. This file tests the verify
//! helper that the future request_body_filter will call.

use bytes::Bytes;
use gw_auth::signature::{verify_required, SignatureCheckResult, SignatureInput};
use gw_codec::compression::Encoding;
use gw_core::entity::SignatureAlgo;
use gw_proxy::body_verify::{verify_body, BodyVerifyInput};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

fn sign(secret: &[u8], method: &str, path: &str, ts: &str, body: &[u8]) -> Vec<u8> {
    let body_hash = hex::encode(Sha256::digest(body));
    let signing = format!("{method}\n{path}\n{ts}\n{body_hash}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string()
}

fn stale_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    (now - 61).to_string()
}

#[test]
fn identity_body_with_valid_signature_passes() {
    let secret = b"s";
    let body = b"{\"hi\":1}";
    let ts = now();
    let sig = sign(secret, "POST", "/v1/chat", &ts, body);

    let input = BodyVerifyInput {
        method: "POST",
        path: "/v1/chat",
        body: Bytes::copy_from_slice(body),
        content_encoding: Encoding::Identity,
        signature_required: true,
        signature_algo: SignatureAlgo::HmacSha256,
        secret,
        signature: Some(sig),
        timestamp: Some(ts),
        max_decoded_bytes: Some(32 * 1024 * 1024),
    };
    let (result, _decoded) = verify_body(input).unwrap();
    assert_eq!(result, SignatureCheckResult::Valid);
}

#[test]
fn gzipped_body_decompressed_then_verified() {
    use flate2::write::GzEncoder;
    use std::io::Write;
    let secret = b"s";
    let plaintext = b"{\"hello\":\"world\"}";
    let ts = now();

    // Client signs the PLAINTEXT body, then gzips it for transport.
    let sig = sign(secret, "POST", "/v1/chat", &ts, plaintext);

    let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(plaintext).unwrap();
    let gz_body = enc.finish().unwrap();

    let input = BodyVerifyInput {
        method: "POST",
        path: "/v1/chat",
        body: Bytes::from(gz_body),
        content_encoding: Encoding::Gzip,
        signature_required: true,
        signature_algo: SignatureAlgo::HmacSha256,
        secret,
        signature: Some(sig),
        timestamp: Some(ts),
        max_decoded_bytes: Some(32 * 1024 * 1024),
    };
    let (result, decoded) = verify_body(input).unwrap();
    assert_eq!(result, SignatureCheckResult::Valid);
    assert_eq!(&decoded[..], plaintext);
}

#[test]
fn not_required_skips_verification() {
    let input = BodyVerifyInput {
        method: "POST",
        path: "/v1/chat",
        body: Bytes::copy_from_slice(b"{}"),
        content_encoding: Encoding::Identity,
        signature_required: false,
        signature_algo: SignatureAlgo::HmacSha256,
        secret: &[],
        signature: None,
        timestamp: None,
        max_decoded_bytes: Some(32 * 1024 * 1024),
    };
    let (result, _decoded) = verify_body(input).unwrap();
    assert_eq!(result, SignatureCheckResult::NotRequired);
}

#[test]
fn bad_signature_rejected() {
    let input = BodyVerifyInput {
        method: "POST",
        path: "/v1/chat",
        body: Bytes::copy_from_slice(b"{}"),
        content_encoding: Encoding::Identity,
        signature_required: true,
        signature_algo: SignatureAlgo::HmacSha256,
        secret: b"s",
        signature: Some(b"deadbeef".to_vec()),
        timestamp: Some(now()),
        max_decoded_bytes: Some(32 * 1024 * 1024),
    };
    let (result, _decoded) = verify_body(input).unwrap();
    assert_eq!(result, SignatureCheckResult::Invalid);
}

#[test]
fn wrong_signature_returns_invalid_result() {
    let body = b"{\"hi\":1}";
    let ts = now();
    let wrong_sig = sign(b"wrong-secret", "POST", "/v1/chat", &ts, body);

    let result = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        b"right-secret",
        Some(&wrong_sig),
        Some(&ts),
        &SignatureInput {
            method: "POST",
            path: "/v1/chat",
            timestamp: &ts,
            body,
        },
    );

    assert_eq!(result, SignatureCheckResult::Invalid);
}

#[test]
fn replay_outside_60_seconds_returns_timestamp_drift() {
    let secret = b"s";
    let body = b"{}";
    let ts = stale_timestamp();
    let sig = sign(secret, "POST", "/v1/chat", &ts, body);

    let input = BodyVerifyInput {
        method: "POST",
        path: "/v1/chat",
        body: Bytes::copy_from_slice(body),
        content_encoding: Encoding::Identity,
        signature_required: true,
        signature_algo: SignatureAlgo::HmacSha256,
        secret,
        signature: Some(sig),
        timestamp: Some(ts),
        max_decoded_bytes: Some(32 * 1024 * 1024),
    };

    let (result, _decoded) = verify_body(input).unwrap();
    assert_eq!(result, SignatureCheckResult::TimestampDrift);
}

#[test]
fn required_missing_signature_returns_missing_but_optional_missing_passes() {
    let required_input = BodyVerifyInput {
        method: "POST",
        path: "/v1/chat",
        body: Bytes::copy_from_slice(b"{}"),
        content_encoding: Encoding::Identity,
        signature_required: true,
        signature_algo: SignatureAlgo::HmacSha256,
        secret: b"s",
        signature: None,
        timestamp: Some(now()),
        max_decoded_bytes: Some(32 * 1024 * 1024),
    };
    let (required_result, _) = verify_body(required_input).unwrap();
    assert_eq!(required_result, SignatureCheckResult::Missing);

    let optional_input = BodyVerifyInput {
        method: "POST",
        path: "/v1/chat",
        body: Bytes::copy_from_slice(b"{}"),
        content_encoding: Encoding::Identity,
        signature_required: false,
        signature_algo: SignatureAlgo::HmacSha256,
        secret: &[],
        signature: None,
        timestamp: None,
        max_decoded_bytes: Some(32 * 1024 * 1024),
    };
    let (optional_result, _) = verify_body(optional_input).unwrap();
    assert_eq!(optional_result, SignatureCheckResult::NotRequired);
}

#[test]
fn compressed_body_rejects_decoded_payload_over_limit() {
    use flate2::write::GzEncoder;
    use std::io::Write;

    let plaintext = vec![b'a'; 64];
    let ts = now();
    let sig = sign(b"s", "POST", "/v1/chat", &ts, &plaintext);

    let mut enc = GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&plaintext).unwrap();
    let gz_body = enc.finish().unwrap();

    let err = verify_body(BodyVerifyInput {
        method: "POST",
        path: "/v1/chat",
        body: Bytes::from(gz_body),
        content_encoding: Encoding::Gzip,
        signature_required: true,
        signature_algo: SignatureAlgo::HmacSha256,
        secret: b"s",
        signature: Some(sig),
        timestamp: Some(ts),
        max_decoded_bytes: Some(plaintext.len() - 1),
    })
    .unwrap_err();

    assert!(err.to_string().contains("decompressed body exceeds"));
}

#[test]
fn malformed_gzip_returns_decode_error() {
    let input = BodyVerifyInput {
        method: "POST",
        path: "/v1/chat",
        body: Bytes::copy_from_slice(b"not gzip data"),
        content_encoding: Encoding::Gzip,
        signature_required: true,
        signature_algo: SignatureAlgo::HmacSha256,
        secret: b"s",
        signature: Some(vec![]),
        timestamp: Some(now()),
        max_decoded_bytes: Some(32 * 1024 * 1024),
    };
    let r = verify_body(input);
    assert!(r.is_err(), "expected gzip decode error");
}
