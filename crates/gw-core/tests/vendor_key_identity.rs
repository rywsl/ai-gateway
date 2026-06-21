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
        model_cost_weights: Default::default(),
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
        quota_state: Default::default(),
    };
}

#[test]
fn vendor_key_struct_literal_sets_model_cost_weights() {
    use gw_core::entity::{KeyStatus, VendorKey};
    use gw_core::ids::{KeyId, ModelName, VendorId};
    use secrecy::SecretString;
    use std::collections::HashMap;

    let mut weights = HashMap::new();
    weights.insert(ModelName::from("gpt-4o"), 900);

    let key = VendorKey {
        key_id: KeyId::from("vk_1"),
        vendor_id: VendorId::from("v_1"),
        api_key: SecretString::new("sk-test".into()),
        accessible_models: vec![ModelName::from("gpt-4o")],
        model_mapping: HashMap::new(),
        model_cost_weights: weights,
        accessible_endpoints: vec![],
        protocol_conversion: true,
        priority: 10,
        weight: 1,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: KeyStatus::Active,
        tags: vec![],
        forced_capabilities: vec![],
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,
        quota_state: Default::default(),
    };

    assert_eq!(
        key.model_cost_weights.get(&ModelName::from("gpt-4o")),
        Some(&900),
    );
}
