use gw_core::entity::VendorKey;
use gw_core::ids::*;
use secrecy::SecretString;

#[test]
fn vendor_key_identity_snapshot_defaults_to_none() {
    let _k = VendorKey {
        key_id: KeyId::from("vk-test"),
        vendor_id: VendorId::from("vendor-test"),
        api_key: SecretString::new("x".into()),
        accessible_models: vec![],
        model_mapping: Default::default(),
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority: 0,
        weight: 1,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: gw_core::entity::KeyStatus::Active,
        tags: vec![],
        forced_capabilities: vec![],
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,
    };
}
