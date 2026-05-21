use gw_auth::signature::{verify_required, SignatureCheckResult, SignatureInput};
use gw_core::entity::SignatureAlgo;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[test]
fn required_false_passes_without_header() {
    let r = verify_required(
        false,
        SignatureAlgo::HmacSha256,
        b"secret",
        None,
        None,
        &SignatureInput {
            method: "POST",
            path: "/v1",
            timestamp: "0",
            body: b"{}",
        },
    );
    assert_eq!(r, SignatureCheckResult::NotRequired);
}

#[test]
fn required_true_missing_header_fails() {
    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        b"secret",
        None,
        None,
        &SignatureInput {
            method: "POST",
            path: "/v1",
            timestamp: "0",
            body: b"{}",
        },
    );
    assert_eq!(r, SignatureCheckResult::Invalid);
}

#[test]
fn required_true_bad_sig_fails() {
    let ts = now().to_string();
    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        b"secret",
        Some(b"deadbeef"),
        Some(&ts),
        &SignatureInput {
            method: "POST",
            path: "/v1",
            timestamp: &ts,
            body: b"{}",
        },
    );
    assert_eq!(r, SignatureCheckResult::Invalid);
}

#[test]
fn required_true_stale_timestamp_fails() {
    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        b"secret",
        Some(b"x"),
        Some("1"),
        &SignatureInput {
            method: "POST",
            path: "/v1",
            timestamp: "1",
            body: b"{}",
        },
    );
    assert_eq!(r, SignatureCheckResult::Invalid);
}

#[test]
fn required_true_valid_passes() {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};

    let secret = b"s";
    let body = b"{\"hi\":1}";
    let ts = now().to_string();
    let body_hash = hex::encode(Sha256::digest(body));
    let signing = format!("POST\n/v1\n{ts}\n{body_hash}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(signing.as_bytes());
    let sig = mac.finalize().into_bytes();

    let r = verify_required(
        true,
        SignatureAlgo::HmacSha256,
        secret,
        Some(&sig),
        Some(&ts),
        &SignatureInput {
            method: "POST",
            path: "/v1",
            timestamp: &ts,
            body,
        },
    );
    assert_eq!(r, SignatureCheckResult::Valid);
}
