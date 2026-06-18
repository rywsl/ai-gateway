//! Protocol version constants for Go/Rust cross-process compatibility.
//!
//! Do not modify these values without considering WS-12 T-06
//! (`scripts/check-rustpact-pv.sh`). Runtime protocol negotiation is wired by
//! WS-8 REV-FINAL-H8.1; these constants are deploy-order guard inputs.

/// Incremented when a Rust/Go rustpact contract gains a field.
pub const RUSTPACT_PROTOCOL_VERSION: u32 = 1;

/// Oldest Go rustpact protocol this Rust build supports.
pub const RUSTPACT_MIN_SUPPORTED_VERSION: u32 = 1;
