//! `AdapterError` — vendor-adapter failure modes.
//!
//! Each variant maps onto a `gw-core::error::codes::GW_*` code at the
//! call site (gw-proxy / gw-task-runtime translate). Adapter code itself
//! stays runtime-agnostic.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("oauth: {0}")]
    Oauth(String),

    #[error("transport rewrite failed: {0}")]
    Transport(String),

    #[error("request patch failed: {0}")]
    PatchRequest(String),

    #[error("response patch failed: {0}")]
    PatchResponse(String),

    #[error("vendor configuration missing: {0}")]
    MissingConfig(&'static str),

    /// Upstream signaled the key is permanently invalid (e.g. revoked
    /// OAuth token discovered during refresh). The caller is responsible
    /// for issuing the K-A cooldown — adapter code stays side-effect-free
    /// (per A-T0 §future-q-7).
    #[error("upstream signaled key invalid")]
    KeyInvalid,
}
