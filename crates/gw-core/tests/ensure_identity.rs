use gw_core::impersonate::{
    ensure_identity, IdentitySnapshot, IdentityVariable, ImpersonateProfile,
};

#[test]
fn ensure_returns_snapshot_when_present_and_matching_profile() {
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::ClaudeCodeChrome131,
        variable: gw_core::impersonate::claude_code::derive("vk-A"),
    };
    let (resolved, persist) =
        ensure_identity("vk-A", Some(&snap), ImpersonateProfile::ClaudeCodeChrome131);
    assert!(matches!(
        resolved.variable,
        IdentityVariable::ClaudeCode { .. }
    ));
    assert!(!persist, "matching snapshot should not request persist");
}

#[test]
fn ensure_re_derives_when_profile_mismatches() {
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::OpenAiCodex,
        variable: gw_core::impersonate::openai_codex::derive("vk-A"),
    };
    let (resolved, persist) =
        ensure_identity("vk-A", Some(&snap), ImpersonateProfile::ClaudeCodeChrome131);
    assert!(matches!(
        resolved.variable,
        IdentityVariable::ClaudeCode { .. }
    ));
    assert!(persist, "mismatched profile must trigger persist");
}

#[test]
fn ensure_derives_when_snapshot_absent_and_requests_persist() {
    let (resolved, persist) = ensure_identity("vk-A", None, ImpersonateProfile::GeminiCli);
    assert!(matches!(
        resolved.variable,
        IdentityVariable::GeminiCli { .. }
    ));
    assert!(persist);
}
