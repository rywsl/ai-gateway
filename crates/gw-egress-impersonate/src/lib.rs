//! In-process BoringSSL/wreq impersonating egress.
//!
//! Spec: `docs/superpowers/specs/2026-05-28-egress-impersonation-design.md`.
//! Pingora rustls (default path) and this crate's boring backend coexist in
//! one binary; this crate runs a loopback HTTP/1 listener that the gateway
//! targets when `Vendor.egress_mode == Impersonate{..}`.

pub mod error;
pub mod handle;
pub mod pool;
pub mod profiles;
pub mod server;
pub mod supervisor;

// ── Public API re-exports ─────────────────────────────────────────────────────

/// The bind address for the loopback egress listener (UDS or TCP).
pub use handle::{EgressBind, EgressHandle};

/// Error classification constants for downstream consumers.
pub use error::{ErrorClass, HDR_CLASS, HDR_DETAIL};
