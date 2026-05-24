//! Body-after HMAC verification + Content-Encoding decode.
//!
//! Called by `filters::request::run` once the full request body has been
//! buffered (the existing 16 MiB body-read loop, before codec parse).
//! Decodes any compressed body first (so the signature always matches the
//! plaintext payload), then runs [`gw_auth::signature::verify_required`].
//!
//! Returns the decoded body alongside the verification result. The caller
//! maps the [`SignatureCheckResult`] variants to distinct 401 error codes
//! per ADR-G7-1 (`GW_AUTH_SIGNATURE_MISSING` / `_INVALID` /
//! `_TIMESTAMP_DRIFT`). Decompression errors are surfaced as
//! [`BodyVerifyError::Decompression`].

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
    pub secret: &'a [u8],
    pub signature: Option<Vec<u8>>,
    pub timestamp: Option<String>,
    pub max_decoded_bytes: Option<usize>,
}

#[derive(Debug, thiserror::Error)]
pub enum BodyVerifyError {
    #[error("body decompression failed: {0}")]
    Decompression(#[from] CompressionError),
    #[error("decompressed body exceeds {limit} bytes")]
    DecodedBodyTooLarge { limit: usize },
}

pub fn verify_body(
    input: BodyVerifyInput<'_>,
) -> Result<(SignatureCheckResult, Bytes), BodyVerifyError> {
    let decoded = decode(input.content_encoding, &input.body)?;
    if let Some(limit) = input.max_decoded_bytes {
        if decoded.len() > limit {
            return Err(BodyVerifyError::DecodedBodyTooLarge { limit });
        }
    }
    let result = verify_required(
        input.signature_required,
        input.signature_algo,
        input.secret,
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
