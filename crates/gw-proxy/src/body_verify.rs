//! Body-after HMAC verification + Content-Encoding decode.
//!
//! Called by `gateway.rs::request_body_filter` once the full request body
//! has been buffered. Decodes any compressed body first (so the signature
//! always matches the plaintext payload), then runs `verify_required`.
//!
//! Returns the decoded body alongside the verification result so the codec
//! parse path can operate on the plaintext.
//
// TODO(T6-C follow-up): wire `verify_body` into
// `gateway.rs::request_body_filter` at EOS. Current state: ctx fields set,
// helper ready. Filter integration deferred to a future task once the
// request_body_filter hot path stabilises.

use bytes::Bytes;
use gw_auth::signature::{verify_required, SignatureCheckResult, SignatureInput};
use gw_codec::compression::{decode, CompressionError, Encoding};
use gw_core::entity::SignatureAlgo;

pub struct BodyVerifyInput<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub body: Bytes,
    pub content_encoding: Encoding,
    pub signature_required: bool,
    pub signature_algo: SignatureAlgo,
    pub secret: Vec<u8>,
    pub signature: Option<Vec<u8>>,
    pub timestamp: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum BodyVerifyError {
    #[error("body decompression failed: {0}")]
    Decompression(#[from] CompressionError),
}

pub fn verify_body(
    input: BodyVerifyInput<'_>,
) -> Result<(SignatureCheckResult, Bytes), BodyVerifyError> {
    let decoded = decode(input.content_encoding, &input.body)?;
    let result = verify_required(
        input.signature_required,
        input.signature_algo,
        &input.secret,
        input.signature.as_deref(),
        input.timestamp.as_deref(),
        &SignatureInput {
            method: input.method,
            path: input.path,
            timestamp: input.timestamp.as_deref().unwrap_or(""),
            body: &decoded,
        },
    );
    Ok((result, decoded))
}
