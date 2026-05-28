//! Map `ImpersonateProfile` → wreq emulation config.
//!
//! # API Deviation from Plan
//! The plan referenced `wreq::EmulationProvider` (which does not exist in 6.0.0-rc.28)
//! and `Emulation::Chrome131.into()` (no `From` impl). The actual API is:
//!   - `wreq_util::Emulation` implements `wreq::EmulationFactory`
//!   - `ClientBuilder::emulation<P: EmulationFactory>(self, p)` accepts it directly
//! So the return type is `wreq_util::Emulation` (not a `wreq::` type), and no conversion
//! is needed — callers pass the return value directly to `.emulation(...)`.

use gw_core::impersonate::ImpersonateProfile;
use wreq_util::Emulation;

/// Returns the wreq emulation factory for the given impersonation profile.
///
/// All three profiles currently pin Chrome 131 per spec §6.3/6.4/6.5.
pub fn emulation_for(profile: ImpersonateProfile) -> Emulation {
    match profile {
        ImpersonateProfile::ClaudeCodeChrome131 => Emulation::Chrome131,
        ImpersonateProfile::OpenAiCodex         => Emulation::Chrome131,
        ImpersonateProfile::GeminiCli           => Emulation::Chrome131,
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
}
