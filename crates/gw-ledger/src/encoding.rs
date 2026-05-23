//! Tiered compression for `usage:detailed` archive payloads.
//!
//! Picks one of three encodings based on uncompressed JSON size; matches the
//! decision table in `docs/superpowers/specs/2026-05-23-detailed-log-complete-design.md`
//! §ADR-D-3.

/// Encoding tag written to the `encoding` field on each Redis stream entry.
/// Consumers branch on this before decompressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Raw,
    Lz4,
    Zstd,
}

impl Encoding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Encoding::Raw => "raw+json",
            Encoding::Lz4 => "lz4+json",
            Encoding::Zstd => "zstd+json",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TierThresholds {
    pub small_threshold_bytes: usize,
    pub medium_threshold_bytes: usize,
}

/// Pick an encoding based on the uncompressed payload length.
pub fn pick_encoding(len: usize, t: &TierThresholds) -> Encoding {
    if len < t.small_threshold_bytes {
        Encoding::Raw
    } else if len < t.medium_threshold_bytes {
        Encoding::Lz4
    } else {
        Encoding::Zstd
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CompressionCfg {
    pub thresholds: TierThresholds,
    pub zstd_level: i32,
}

#[derive(Debug)]
pub enum EncodeError {
    /// `zstd` returned an error. `lz4_flex` infallibly encodes; `Raw` never errors.
    Zstd(std::io::Error),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Zstd(e) => write!(f, "zstd encode failed: {e}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encode a JSON-serialized payload. The caller owns input; we return the
/// (possibly identical) compressed bytes. `Raw` returns the input unchanged.
pub fn encode(payload: Vec<u8>, cfg: &CompressionCfg) -> Result<(Encoding, Vec<u8>), EncodeError> {
    let enc = pick_encoding(payload.len(), &cfg.thresholds);
    let out = match enc {
        Encoding::Raw => payload,
        Encoding::Lz4 => lz4_flex::compress_prepend_size(&payload),
        Encoding::Zstd => {
            zstd::encode_all(payload.as_slice(), cfg.zstd_level).map_err(EncodeError::Zstd)?
        }
    };
    Ok((enc, out))
}

#[derive(Debug)]
pub enum DecodeError {
    Lz4(lz4_flex::block::DecompressError),
    Zstd(std::io::Error),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Lz4(e) => write!(f, "lz4 decode failed: {e}"),
            DecodeError::Zstd(e) => write!(f, "zstd decode failed: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Test-side helper that inverts `encode`. Production code in this crate
/// never decodes; only the Redis-backed integration test does.
pub fn decode(enc: Encoding, bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
    match enc {
        Encoding::Raw => Ok(bytes.to_vec()),
        Encoding::Lz4 => lz4_flex::decompress_size_prepended(bytes).map_err(DecodeError::Lz4),
        Encoding::Zstd => zstd::decode_all(bytes).map_err(DecodeError::Zstd),
    }
}
