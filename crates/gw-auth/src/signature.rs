//! HMAC-SHA256 signature verification with constant-time comparison.
//!
//! Canonical signing string is:
//! ```text
//! <method>\n<path>\n<timestamp>\n<sha256(body) as lowercase hex>
//! ```

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

pub struct SignatureInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub timestamp: &'a str,
    pub body: &'a [u8],
}

pub fn verify(secret: &[u8], received: &[u8], input: &SignatureInput<'_>) -> bool {
    let body_hash = {
        use sha2::Digest;
        let mut h = Sha256::new();
        h.update(input.body);
        hex::encode(h.finalize())
    };
    let signing = format!(
        "{}\n{}\n{}\n{}",
        input.method, input.path, input.timestamp, body_hash
    );
    let mut mac = match Hmac::<Sha256>::new_from_slice(secret) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(signing.as_bytes());
    let expected = mac.finalize().into_bytes();
    expected.ct_eq(received).into()
}
