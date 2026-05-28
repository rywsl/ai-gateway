//! Per-vendor impersonation profile types and per-VendorKey identity derivation.
//!
//! Pure-types layer (no IO / no async). The `gw-egress-impersonate` crate
//! maps each profile to a concrete `rquest::Emulation` + `Http2Config`; this
//! module is consumed by both `gw-egress-impersonate` (profile metadata)
//! and `gw-proxy` (write the forged headers in `inject_vendor_headers`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod rng;
pub mod claude_code;
pub mod openai_codex;

/// Identifies which official upstream CLI client to imitate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpersonateProfile {
    /// Anthropic — Claude Code (Stainless SDK on Node + Electron Chrome 131 fingerprint).
    ClaudeCodeChrome131,
    /// OpenAI — Codex CLI (codex_cli_rs on Chromium-class TLS).
    #[serde(rename = "openai_codex")]
    OpenAiCodex,
    /// Google — Gemini CLI (Node + undici, impersonated as Chrome 131 for higher pass rate).
    GeminiCli,
}

impl ImpersonateProfile {
    pub fn as_static_str(self) -> &'static str {
        match self {
            Self::ClaudeCodeChrome131 => "claude_code_chrome131",
            Self::OpenAiCodex => "openai_codex",
            Self::GeminiCli => "gemini_cli",
        }
    }
}

impl std::str::FromStr for ImpersonateProfile {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude_code_chrome131" => Ok(Self::ClaudeCodeChrome131),
            "openai_codex" => Ok(Self::OpenAiCodex),
            "gemini_cli" => Ok(Self::GeminiCli),
            other => Err(format!("unknown ImpersonateProfile: {other}")),
        }
    }
}

/// Per-Vendor egress selection. `Rustls` is the default and preserves the
/// existing Pingora-rustls upstream path bit-for-bit; `Impersonate` routes
/// through the in-process boring egress (gw-egress-impersonate).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum VendorEgressMode {
    Rustls,
    Impersonate { profile: ImpersonateProfile },
}

impl Default for VendorEgressMode {
    fn default() -> Self {
        Self::Rustls
    }
}

/// Persisted per-VendorKey impersonation identity. Written on first
/// impersonated dispatch (lazy persist) and reused for every subsequent
/// request from the same key so the upstream sees a stable fingerprint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentitySnapshot {
    pub profile_kind: ImpersonateProfile,
    pub variable: IdentityVariable,
}

/// Profile-specific identity payload. The variant chosen by the VendorKey's
/// owning Vendor — must agree with `IdentitySnapshot.profile_kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "profile", rename_all = "snake_case")]
pub enum IdentityVariable {
    ClaudeCode {
        os: String,
        arch: String,
        node_version: String,
        package_version: String,
        timeout: String,
        chrome_version: String,
        electron_version: String,
        user_agent: String,
    },
    #[serde(rename = "openai_codex")]
    OpenAiCodex {
        codex_version: String,
        os: String,
        arch: String,
        installation_id: Uuid,
        user_agent: String,
    },
    GeminiCli {
        cli_version: String,
        os_type: String,
        arch: String,
        installation_id: Uuid,
    },
}
