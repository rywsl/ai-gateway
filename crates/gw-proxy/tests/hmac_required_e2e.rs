//! Body-after HMAC verification helper.
//!
//! Real e2e (with full Pingora session + wiremock) lives in
//! e2e_failure_modes.rs once T6-C wiring lands. This file tests the verify
//! helper that the future request_body_filter will call.

use bytes::Bytes;
use gw_auth::signature::SignatureCheckResult;
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
        secret: secret.to_vec(),
        signature: Some(sig),
        timestamp: Some(ts),
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
        secret: secret.to_vec(),
        signature: Some(sig),
        timestamp: Some(ts),
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
        secret: vec![],
        signature: None,
        timestamp: None,
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
        secret: b"s".to_vec(),
        signature: Some(b"deadbeef".to_vec()),
        timestamp: Some(now()),
    };
    let (result, _decoded) = verify_body(input).unwrap();
    assert_eq!(result, SignatureCheckResult::Invalid);
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
        secret: b"s".to_vec(),
        signature: Some(vec![]),
        timestamp: Some(now()),
    };
    let r = verify_body(input);
    assert!(r.is_err(), "expected gzip decode error");
}
