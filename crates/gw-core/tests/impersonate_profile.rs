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

use gw_core::impersonate::{IdentitySnapshot, IdentityVariable};
use uuid::Uuid;

#[test]
fn identity_snapshot_claude_code_roundtrip() {
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::ClaudeCodeChrome131,
        variable: IdentityVariable::ClaudeCode {
            os: "Linux".into(),
            arch: "arm64".into(),
            node_version: "v22.14.0".into(),
            package_version: "0.70.0".into(),
            timeout: "600".into(),
            chrome_version: "131.0.6778.265".into(),
            electron_version: "37.3.0".into(),
            user_agent: "Mozilla/5.0 (...)".into(),
        },
    };
    let j = serde_json::to_value(&snap).unwrap();
    assert_eq!(j["profile_kind"], "claude_code_chrome131");
    assert_eq!(j["variable"]["profile"], "claude_code");
    let back: IdentitySnapshot = serde_json::from_value(j).unwrap();
    assert_eq!(back.profile_kind, snap.profile_kind);
}

#[test]
fn identity_snapshot_openai_codex_roundtrip() {
    let id = Uuid::new_v4();
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::OpenAiCodex,
        variable: IdentityVariable::OpenAiCodex {
            codex_version: "0.4.2".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
            installation_id: id,
            user_agent: "codex_cli_rs/0.4.2 (linux; x86_64)".into(),
        },
    };
    let j = serde_json::to_value(&snap).unwrap();
    assert_eq!(j["profile_kind"], "openai_codex");
    assert_eq!(j["variable"]["profile"], "openai_codex");

    let s = serde_json::to_string(&snap).unwrap();
    let back: IdentitySnapshot = serde_json::from_str(&s).unwrap();
    match back.variable {
        IdentityVariable::OpenAiCodex { installation_id, .. } => assert_eq!(installation_id, id),
        _ => panic!("variant mismatch"),
    }
}

#[test]
fn identity_snapshot_gemini_cli_roundtrip() {
    let id = Uuid::new_v4();
    let snap = IdentitySnapshot {
        profile_kind: ImpersonateProfile::GeminiCli,
        variable: IdentityVariable::GeminiCli {
            cli_version: "0.1.21".into(),
            os_type: "linux".into(),
            arch: "x64".into(),
            installation_id: id,
        },
    };
    let s = serde_json::to_string(&snap).unwrap();
    let back: IdentitySnapshot = serde_json::from_str(&s).unwrap();
    assert_eq!(back.profile_kind, ImpersonateProfile::GeminiCli);
}

