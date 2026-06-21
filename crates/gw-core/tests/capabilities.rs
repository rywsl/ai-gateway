//! T0-C serde + struct shape: the new capability fields default cleanly so
//! existing payloads (and existing in-memory `VendorKey` constructions in
//! the loader) keep compiling without migration in user code.
//!
//! `Model` is `Deserialize`, so we exercise the JSON path directly.
//! `VendorKey` is NOT `Deserialize` (it is hand-assembled from a
//! `VendorKeyRow` in `gw-store::loader`, which is intentionally out of
//! T0-C's allowlist), so we exercise the struct-literal path to confirm
//! the new fields exist with the documented types and that the loader can
//! plug `Vec::new()` / `false` defaults until T3-C wires them up.

use gw_core::entity::{KeyStatus, Model, VendorKey};
use gw_core::ids::{KeyId, VendorId};
use gw_core::protocol::ProtocolKind;
use secrecy::SecretString;
use std::collections::HashMap;

#[test]
fn model_capabilities_defaults_empty() {
    // Minimal payload — relies on serde(default) for `capabilities`.
    let json = r#"{"name":"gpt-4"}"#;
    let m: Model = serde_json::from_str(json).unwrap();
    assert!(m.capabilities.is_empty());
    assert_eq!(m.name.0, "gpt-4");
}

#[test]
fn model_capabilities_round_trip() {
    let json = r#"{"name":"gpt-4o","capabilities":["vision","thinking"]}"#;
    let m: Model = serde_json::from_str(json).unwrap();
    assert_eq!(
        m.capabilities,
        vec!["vision".to_string(), "thinking".to_string()]
    );

    // Round-trip back to JSON and re-parse to confirm Serialize is symmetric.
    let s = serde_json::to_string(&m).unwrap();
    let m2: Model = serde_json::from_str(&s).unwrap();
    assert_eq!(m2.capabilities, m.capabilities);
    assert_eq!(m2.name.0, "gpt-4o");
}

#[test]
fn vendor_key_capability_fields_default_construction() {
    // `VendorKey` is hand-assembled from `VendorKeyRow` in gw-store::loader;
    // updating that intermediate is T3-C's responsibility. Here we only
    // confirm that the new fields exist on the struct with the documented
    // types so the loader can default them to `Vec::new()` / `false` and
    // existing call-sites compile unchanged.
    let vk = VendorKey {
        key_id: KeyId("k1".into()),
        vendor_id: VendorId("v1".into()),
        api_key: SecretString::from(String::from("plaintext-secret")),
        accessible_models: Vec::new(),
        model_mapping: HashMap::new(),
        model_cost_weights: Default::default(),
        accessible_endpoints: Vec::<ProtocolKind>::new(),
        protocol_conversion: false,
        priority: 0,
        weight: 0,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: KeyStatus::Active,
        tags: Vec::new(),
        forced_capabilities: Vec::new(),
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
        identity_snapshot: None,
        quota_state: Default::default(),
    };

    assert!(vk.forced_capabilities.is_empty());
    assert!(!vk.exclusive_capability_match);
    assert!(vk.tunnel_preference.is_none());
    assert!(vk.oauth.is_none());
}
