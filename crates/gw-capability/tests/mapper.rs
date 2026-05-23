use gw_capability::mapper::{FakeMapperBackend, MappingSource, ModelMapper};
use gw_core::entity::{ModelAvailability, ModelMapping};
use gw_core::protocol::ProtocolKind;
use std::collections::BTreeMap;
use std::sync::Arc;

fn empty_overrides() -> BTreeMap<String, String> {
    BTreeMap::new()
}

#[tokio::test]
async fn entity_row_returns_default_upstream() {
    let be = FakeMapperBackend::default();
    be.insert(ModelMapping {
        user_alias: "claude-3-opus".into(),
        protocol: ProtocolKind::Anthropic,
        default_upstream_model: "claude-3-opus-20240229".into(),
        vendor_key_overrides: empty_overrides(),
        availability: ModelAvailability::Enabled,
        pricing_strategy_ref: None,
    });
    let m = ModelMapper::new(Arc::new(be));
    let mapped = m
        .map_for_routing("claude-3-opus", ProtocolKind::Anthropic)
        .await
        .unwrap();
    assert_eq!(mapped.upstream_model, "claude-3-opus-20240229");
    assert_eq!(mapped.source, MappingSource::EntityRow);
}

#[tokio::test]
async fn identity_when_no_mapping() {
    let be = FakeMapperBackend::default();
    let m = ModelMapper::new(Arc::new(be));
    let mapped = m
        .map_for_routing("unknown-model", ProtocolKind::OpenAi)
        .await
        .unwrap();
    assert_eq!(mapped.upstream_model, "unknown-model");
    assert_eq!(mapped.source, MappingSource::Identity);
}

#[tokio::test]
async fn per_key_when_vendor_overrides_present() {
    let be = FakeMapperBackend::default();
    let mut overrides = BTreeMap::new();
    overrides.insert("vk-1".into(), "gpt-5-2026".into());
    be.insert(ModelMapping {
        user_alias: "gpt-5".into(),
        protocol: ProtocolKind::OpenAi,
        default_upstream_model: "gpt-5".into(),
        vendor_key_overrides: overrides,
        availability: ModelAvailability::Enabled,
        pricing_strategy_ref: None,
    });
    let m = ModelMapper::new(Arc::new(be));
    let mapped = m
        .map_for_routing("gpt-5", ProtocolKind::OpenAi)
        .await
        .unwrap();
    assert_eq!(mapped.source, MappingSource::PerKey);
}

#[tokio::test]
async fn disabled_returns_error() {
    let be = FakeMapperBackend::default();
    be.insert(ModelMapping {
        user_alias: "deprecated".into(),
        protocol: ProtocolKind::OpenAi,
        default_upstream_model: "x".into(),
        vendor_key_overrides: empty_overrides(),
        availability: ModelAvailability::Disabled,
        pricing_strategy_ref: None,
    });
    let m = ModelMapper::new(Arc::new(be));
    let err = m
        .map_for_routing("deprecated", ProtocolKind::OpenAi)
        .await
        .unwrap_err();
    assert!(format!("{err:?}").to_lowercase().contains("disabled"));
}

#[tokio::test]
async fn map_for_dispatch_uses_override_when_key_match() {
    let be = FakeMapperBackend::default();
    let mut overrides = BTreeMap::new();
    overrides.insert("vk-special".into(), "claude-3-opus-special".into());
    be.insert(ModelMapping {
        user_alias: "claude-3-opus".into(),
        protocol: ProtocolKind::Anthropic,
        default_upstream_model: "claude-3-opus-20240229".into(),
        vendor_key_overrides: overrides,
        availability: ModelAvailability::Enabled,
        pricing_strategy_ref: None,
    });
    let m = ModelMapper::new(Arc::new(be));
    let r = m
        .map_for_dispatch("claude-3-opus", ProtocolKind::Anthropic, "vk-special")
        .await;
    assert_eq!(r, "claude-3-opus-special");
    let r2 = m
        .map_for_dispatch("claude-3-opus", ProtocolKind::Anthropic, "vk-other")
        .await;
    assert_eq!(r2, "claude-3-opus-20240229");
}
