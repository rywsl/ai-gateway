//! 入站解压 + 出站协商 + 出向压缩。
//!
//! v1 supports gzip and br; zstd is unrecognized and degrades to identity.
//! SSE responses must always remain identity (caller responsibility).

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
}

pub fn decode(enc: Encoding, data: &[u8]) -> Result<Bytes, CompressionError> {
    match enc {
        Encoding::Identity => Ok(Bytes::copy_from_slice(data)),
        Encoding::Gzip => {
            let mut d = flate2::read::GzDecoder::new(data);
            let mut out = Vec::with_capacity(data.len().saturating_mul(4));
            d.read_to_end(&mut out)?;
            Ok(Bytes::from(out))
        }
        Encoding::Br => {
            let mut out = Vec::with_capacity(data.len().saturating_mul(4));
            brotli::Decompressor::new(data, 4096).read_to_end(&mut out)?;
            Ok(Bytes::from(out))
        }
    }
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
