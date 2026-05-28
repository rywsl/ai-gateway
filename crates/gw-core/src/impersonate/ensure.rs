use crate::impersonate::{
    claude_code, gemini_cli, openai_codex, IdentitySnapshot, IdentityVariable, ImpersonateProfile,
};

/// Resolved identity for the current request.
pub struct ResolvedIdentity {
    pub profile: ImpersonateProfile,
    pub variable: IdentityVariable,
}

/// Returns the resolved identity AND a boolean indicating whether the caller
/// SHOULD schedule a lazy persist of `IdentitySnapshot { profile, variable }`
/// to `vendor_keys.identity_snapshot`.
pub fn ensure_identity(
    key_id: &str,
    existing: Option<&IdentitySnapshot>,
    profile: ImpersonateProfile,
) -> (ResolvedIdentity, bool) {
    if let Some(snap) = existing {
        if snap.profile_kind == profile {
            return (
                ResolvedIdentity {
                    profile,
                    variable: snap.variable.clone(),
                },
                false,
            );
        }
        // profile drift → re-derive
    }
    let variable = match profile {
        ImpersonateProfile::ClaudeCodeChrome131 => claude_code::derive(key_id),
        ImpersonateProfile::OpenAiCodex => openai_codex::derive(key_id),
        ImpersonateProfile::GeminiCli => gemini_cli::derive(key_id),
    };
    (ResolvedIdentity { profile, variable }, true)
}
