use gw_core::entity::Vendor;
use gw_core::impersonate::{ImpersonateProfile, VendorEgressMode};

#[test]
fn vendor_egress_mode_defaults_to_rustls_when_absent() {
    let json = r#"{
        "id":"vendor-test",
        "display_name":"Test",
        "base_urls":["https://example.com/"],
        "auth_template":"Bearer {api_key}"
    }"#;
    let v: Vendor = serde_json::from_str(json).unwrap();
    assert!(matches!(v.egress_mode, VendorEgressMode::Rustls));
}

#[test]
fn vendor_egress_mode_round_trips_impersonate() {
    let json = r#"{
        "id":"vendor-test",
        "display_name":"Test",
        "base_urls":["https://example.com/"],
        "auth_template":"Bearer {api_key}",
        "egress_mode":{"mode":"impersonate","profile":"claude_code_chrome131"}
    }"#;
    let v: Vendor = serde_json::from_str(json).unwrap();
    match v.egress_mode {
        VendorEgressMode::Impersonate { profile } => {
            assert_eq!(profile, ImpersonateProfile::ClaudeCodeChrome131);
        }
        _ => panic!("expected Impersonate"),
    }
}
