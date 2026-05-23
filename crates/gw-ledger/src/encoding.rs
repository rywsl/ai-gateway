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
