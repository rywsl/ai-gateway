use gw_core::impersonate::ImpersonateProfile;

#[test]
fn impersonate_profile_serde_snake_case() {
    let serialized = serde_json::to_string(&ImpersonateProfile::ClaudeCodeChrome131).unwrap();
    assert_eq!(serialized, "\"claude_code_chrome131\"");

    let parsed: ImpersonateProfile = serde_json::from_str("\"openai_codex\"").unwrap();
    assert_eq!(parsed, ImpersonateProfile::OpenAiCodex);
}

#[test]
fn impersonate_profile_as_static_str() {
    assert_eq!(ImpersonateProfile::ClaudeCodeChrome131.as_static_str(), "claude_code_chrome131");
    assert_eq!(ImpersonateProfile::OpenAiCodex.as_static_str(), "openai_codex");
    assert_eq!(ImpersonateProfile::GeminiCli.as_static_str(), "gemini_cli");
}
