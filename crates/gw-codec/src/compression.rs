//! 入站解压 + 出站协商 + 出向压缩。
//!
//! v1 supports gzip and br; zstd is unrecognized and degrades to identity.
//! SSE responses must always remain identity (caller responsibility).
//!
//! ## Decompression bomb protection
//!
//! `decode` enforces a `max_bytes` ceiling **inside the decode loop** via
//! `Read::take(max_bytes + 1)`. flate2 / brotli decompress into the caller's
//! buffer one chunk at a time, so capping the `Read` adapter bounds peak
//! resident allocation at `max_bytes + 1 + small_internal_buffer` — a 1 MiB
//! brotli bomb that would inflate to multiple GiB is rejected before any
//! oversized `Vec` allocation happens.

use bytes::Bytes;
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Identity,
    Gzip,
    Br,
}

#[derive(Debug, thiserror::Error)]
pub enum CompressionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("brotli: {0}")]
    Brotli(String),
    #[error("decoded size exceeds limit of {limit} bytes")]
    TooLarge { limit: usize },
}

/// Decode a possibly-compressed payload, refusing to inflate past `max_bytes`.
///
/// `max_bytes` is a streaming ceiling: the decoder is wrapped in
/// `Read::take(max_bytes + 1)` so peak memory is bounded even on a malicious
/// compression bomb. A payload that would inflate above the limit returns
/// [`CompressionError::TooLarge`].
pub fn decode(enc: Encoding, data: &[u8], max_bytes: usize) -> Result<Bytes, CompressionError> {
    match enc {
        Encoding::Identity => {
            if data.len() > max_bytes {
                return Err(CompressionError::TooLarge { limit: max_bytes });
            }
            Ok(Bytes::copy_from_slice(data))
        }
        Encoding::Gzip => {
            decode_bounded(flate2::read::GzDecoder::new(data), data.len(), max_bytes)
        }
        Encoding::Br => decode_bounded(
            brotli::Decompressor::new(data, 4096),
            data.len(),
            max_bytes,
        ),
    }
}

/// Stream `reader` into a `Vec`, refusing to grow beyond `max_bytes`.
fn decode_bounded<R: Read>(
    reader: R,
    input_len: usize,
    max_bytes: usize,
) -> Result<Bytes, CompressionError> {
    // Pre-allocate up to ~4× input but never above max_bytes — a 1 KiB input
    // shouldn't reserve gigabytes preemptively even when the cap is high.
    let cap = input_len.saturating_mul(4).min(max_bytes);
    let mut out = Vec::with_capacity(cap);
    // `Take(max_bytes + 1)` lets read_to_end consume *at most* max_bytes + 1
    // bytes; if the decoder produces more we cut off and Err. The +1 lets us
    // distinguish "exactly at the limit" (OK) from "over the limit" (Err).
    let limit = (max_bytes as u64).saturating_add(1);
    reader.take(limit).read_to_end(&mut out)?;
    if out.len() > max_bytes {
        return Err(CompressionError::TooLarge { limit: max_bytes });
    }
    Ok(Bytes::from(out))
}

pub fn encode(enc: Encoding, data: &[u8]) -> Result<Bytes, CompressionError> {
    match enc {
        Encoding::Identity => Ok(Bytes::copy_from_slice(data)),
        Encoding::Gzip => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(data)?;
            Ok(Bytes::from(e.finish()?))
        }
        Encoding::Br => {
            let mut out = Vec::new();
            let params = brotli::enc::BrotliEncoderParams::default();
            brotli::BrotliCompress(&mut std::io::Cursor::new(data), &mut out, &params)
                .map_err(|e| CompressionError::Brotli(format!("{e:?}")))?;
            Ok(Bytes::from(out))
        }
    }
}

pub fn parse_content_encoding(header: Option<&str>) -> Encoding {
    match header.map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("gzip") => Encoding::Gzip,
        Some(s) if s.eq_ignore_ascii_case("br") => Encoding::Br,
        _ => Encoding::Identity,
    }
}

/// Negotiate outbound encoding from a comma-separated `Accept-Encoding` value.
/// Preference order: br > gzip > identity. Unknown values (zstd, deflate)
/// fall through to identity.
pub fn negotiate_outbound(accept: &str) -> Encoding {
    let lower = accept.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split(',').map(str::trim).collect();
    if parts.iter().any(|p| p.starts_with("br")) {
        Encoding::Br
    } else if parts.iter().any(|p| p.starts_with("gzip")) {
        Encoding::Gzip
    } else {
        Encoding::Identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gzip_encode(payload: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(payload).unwrap();
        e.finish().unwrap()
    }

    fn brotli_encode(payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let params = brotli::enc::BrotliEncoderParams::default();
        brotli::BrotliCompress(&mut std::io::Cursor::new(payload), &mut out, &params).unwrap();
        out
    }

    #[test]
    fn decode_identity_within_cap() {
        let r = decode(Encoding::Identity, b"hello", 1024).unwrap();
        assert_eq!(r.as_ref(), b"hello");
    }

    #[test]
    fn decode_identity_rejects_over_cap() {
        let err = decode(Encoding::Identity, b"hello world", 5).unwrap_err();
        assert!(matches!(err, CompressionError::TooLarge { limit: 5 }));
    }

    #[test]
    fn decode_gzip_within_cap() {
        let plain = b"some payload";
        let enc = gzip_encode(plain);
        let r = decode(Encoding::Gzip, &enc, 1024).unwrap();
        assert_eq!(r.as_ref(), &plain[..]);
    }

    #[test]
    fn decode_brotli_within_cap() {
        let plain = b"some payload";
        let enc = brotli_encode(plain);
        let r = decode(Encoding::Br, &enc, 1024).unwrap();
        assert_eq!(r.as_ref(), &plain[..]);
    }

    /// 4 MiB of zero bytes gzip-compresses to ~4 KiB. Capping at 1 MiB must
    /// refuse to inflate it past the limit and never allocate the full 4 MiB.
    #[test]
    fn decode_gzip_bomb_rejected_within_cap() {
        let plain = vec![0u8; 4 * 1024 * 1024];
        let bomb = gzip_encode(&plain);
        assert!(bomb.len() < 64 * 1024, "compresses to <64KiB");
        let err = decode(Encoding::Gzip, &bomb, 1024 * 1024).unwrap_err();
        assert!(matches!(err, CompressionError::TooLarge { limit } if limit == 1024 * 1024));
    }

    /// 16 MiB of zero bytes brotli-compresses to a few hundred bytes.
    #[test]
    fn decode_brotli_bomb_rejected_within_cap() {
        let plain = vec![0u8; 16 * 1024 * 1024];
        let bomb = brotli_encode(&plain);
        assert!(bomb.len() < 4 * 1024, "brotli compresses zeros tightly");
        let err = decode(Encoding::Br, &bomb, 1024 * 1024).unwrap_err();
        assert!(matches!(err, CompressionError::TooLarge { limit } if limit == 1024 * 1024));
    }

    /// Exactly-at-the-limit case: must accept, not reject.
    #[test]
    fn decode_gzip_exactly_at_cap_ok() {
        let plain = vec![0u8; 1024];
        let enc = gzip_encode(&plain);
        let r = decode(Encoding::Gzip, &enc, 1024).unwrap();
        assert_eq!(r.len(), 1024);
    }

    #[test]
    fn negotiate_outbound_prefers_br_over_gzip() {
        assert_eq!(negotiate_outbound("gzip, br"), Encoding::Br);
        assert_eq!(negotiate_outbound("gzip"), Encoding::Gzip);
        assert_eq!(negotiate_outbound("identity"), Encoding::Identity);
        assert_eq!(negotiate_outbound("zstd"), Encoding::Identity);
    }
}
