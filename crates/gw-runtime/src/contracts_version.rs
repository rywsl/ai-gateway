//! Protocol version constants for Go/Rust cross-process compatibility.
//!
//! Do not modify these values without considering WS-12 T-06
//! (`scripts/check-rustpact-pv.sh`) and WS-12 T-07b
//! (`scripts/check-deploy-order.sh`). Runtime protocol negotiation is wired by
//! WS-8 REV-FINAL-H8.1; these constants are deploy-order guard inputs.
//!
//! M10/M11 (v2.1 §11) red lines: do NOT touch `crates/gw-ledger/src/coef.rs`
//! or `coef_snapshot.rs`. The constants here are deploy-order guards only.

/// Legacy name kept for `scripts/check-rustpact-pv.sh` and
/// `scripts/check-deploy-order.sh` which grep this symbol. Bumped in lockstep
/// with `CURRENT_PV` (WS-8). New code should prefer `CURRENT_PV`.
pub const RUSTPACT_PROTOCOL_VERSION: u32 = 3;

/// Legacy name kept for `scripts/check-deploy-order.sh`. New code should
/// prefer `MIN_SUPPORTED_PV`.
pub const RUSTPACT_MIN_SUPPORTED_VERSION: u32 = 1;

/// Current protocol version emitted by new `UsageSnapshot` records. Bump in
/// lockstep with Go `shared/rustpact/version.go::RustpactCurrentPV` and
/// `crates/gw-core/src/usage.rs::UsageSnapshot::CURRENT_PV`. WS-8 REV-FINAL-H8.1.
pub const CURRENT_PV: u32 = 3;

/// Oldest rustpact protocol version this Rust build accepts. Stays at 1 so
/// pre-WS-8 records continue to decode during the rollout. Mirrors Go
/// `RustpactMinSupportedPV`.
pub const MIN_SUPPORTED_PV: u32 = 1;
