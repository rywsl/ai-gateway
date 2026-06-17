//! Map `ImpersonateProfile` → wreq TLS/HTTP-2 emulation config.
//!
//! # API Deviation from Plan
//!
//! The plan referenced `wreq::EmulationProvider` (which does not exist in 6.0.0-rc.28)
//! and `Emulation::Chrome131.into()` (no `From` impl). The actual API is:
//!
//! - `wreq_util::Emulation` implements `wreq::EmulationFactory`
//! - `ClientBuilder::emulation<P: EmulationFactory>(self, p)` accepts it directly
//!
//! So the return type is `wreq_util::Emulation` (not a `wreq::` type), and no
//! conversion is needed — callers pass the return value directly to `.emulation(...)`.
//!
//! # D14 (#18) — profile TLS emulation is deliberately differentiated
//!
//! The pinned `wreq-util = 3.0.0-rc.11` exposes browser-family emulations
//! (Chrome/Edge/Opera/Firefox/Safari/OkHttp) but no Node/undici-specific
//! profile. D14's short-term fix is therefore to keep the Chrome-class family
//! while avoiding one identical JA3/JA4 for every vendor profile:
//!
//! - **Claude Code** ships as an Electron app — a genuine Chromium 131 client, so
//!   the Chrome 131 JA3/JA4 is the *correct* fingerprint, not an impersonation.
//! - **OpenAI Codex** (`codex_cli_rs`) presents a Chromium-class TLS stack; use a
//!   nearby Chrome 133 emulation so it no longer shares Claude Code's exact TLS
//!   fingerprint.
//! - **Gemini CLI** is really Node + undici (not Chrome). It is *deliberately
//!   over-impersonated* with a Chrome-class fingerprint for Google edge pass
//!   rate, but uses Chrome 132 so it is not byte-identical to the other profiles.
//!
//! The per-identity / static-header layer IS already fully differentiated per
//! profile in `gw_core::impersonate::{claude_code, openai_codex, gemini_cli}` —
//! this module provides the TLS/HTTP2 layer. If Google starts blocking
//! Chrome-class Gemini CLI traffic, add a first-class Node/undici emulation when
//! `wreq-util` exposes one and update the known-gaps doc.

use gw_core::impersonate::ImpersonateProfile;
use wreq_util::Emulation;

/// Returns the wreq emulation factory for the given impersonation profile.
///
/// See the module-level docs (D14) for the profile-specific mapping rationale.
pub fn emulation_for(profile: ImpersonateProfile) -> Emulation {
    match profile {
        // Electron-based Claude Code: genuinely Chromium 131.
        ImpersonateProfile::ClaudeCodeChrome131 => Emulation::Chrome131,
        // Codex CLI: Chromium-class TLS, intentionally not the same JA3/JA4 as Claude Code.
        ImpersonateProfile::OpenAiCodex => Emulation::Chrome133,
        // Gemini CLI: Node+undici in the wild, deliberately over-impersonated as
        // Chrome-class until wreq-util exposes a Node/undici profile.
        ImpersonateProfile::GeminiCli => Emulation::Chrome132,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::impersonate::ImpersonateProfile;

    #[test]
    fn all_profiles_have_emulation() {
        for p in [
            ImpersonateProfile::ClaudeCodeChrome131,
            ImpersonateProfile::OpenAiCodex,
            ImpersonateProfile::GeminiCli,
        ] {
            let _ = emulation_for(p);
        }
    }

    #[test]
    fn profiles_do_not_share_one_tls_emulation() {
        assert_eq!(
            emulation_for(ImpersonateProfile::ClaudeCodeChrome131),
            Emulation::Chrome131
        );
        assert_eq!(
            emulation_for(ImpersonateProfile::OpenAiCodex),
            Emulation::Chrome133
        );
        assert_eq!(
            emulation_for(ImpersonateProfile::GeminiCli),
            Emulation::Chrome132
        );
    }
}
