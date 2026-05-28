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

use gw_core::impersonate::VendorEgressMode;

#[test]
fn vendor_egress_mode_default_is_rustls() {
    let default: VendorEgressMode = Default::default();
    assert!(matches!(default, VendorEgressMode::Rustls));
}

#[test]
fn vendor_egress_mode_serde_tagged() {
    let m = VendorEgressMode::Impersonate {
        profile: ImpersonateProfile::ClaudeCodeChrome131,
    };
    let j = serde_json::to_value(&m).unwrap();
    assert_eq!(j["mode"], "impersonate");
    assert_eq!(j["profile"], "claude_code_chrome131");

    let back: VendorEgressMode = serde_json::from_value(j).unwrap();
    assert!(matches!(
        back,
        VendorEgressMode::Impersonate { profile: ImpersonateProfile::ClaudeCodeChrome131 }
    ));

    let rust: VendorEgressMode = serde_json::from_str(r#"{"mode":"rustls"}"#).unwrap();
    assert!(matches!(rust, VendorEgressMode::Rustls));
}

