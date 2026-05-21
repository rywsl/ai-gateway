//! T3-C (G13) — `CapabilityResolver` + candidate capability filter.
//!
//! Two layers under test:
//! 1. `CapabilityResolver::resolve` collapses (key-forced + model + header)
//!    caps into a `RequiredCapabilities` set with the right `MatchMode`.
//! 2. `get_candidate_keys` capability-filters candidate `VendorKey`s via
//!    `gw_capability::candidate_matches` (channels advertise
//!    `forced_capabilities` + `exclusive_capability_match`).
//!
//! All in-memory: `FakeResolveBackend` for (1), an `EntityStore::new()` seeded
//! with `vendor_key` rows for (2). No Docker / Postgres required.

use gw_capability::resolver::{CapabilityResolver, FakeResolveBackend, ResolveRequest};
use gw_capability::types::{Capability, MatchMode};
use gw_core::entity::*;
use gw_core::ids::*;
use gw_core::permissions::MergedPermissions;
use gw_core::protocol::ProtocolKind;
use gw_core::versioned::Versioned;
use gw_routing::candidates::get_candidate_keys;
use gw_routing::in_flight::InFlightStore;
use gw_store::EntityStore;
use secrecy::SecretString;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

// ----------------------------------------------------------------------------
// Resolver layer
// ----------------------------------------------------------------------------

#[tokio::test]
async fn resolve_pulls_model_caps() {
    let backend = FakeResolveBackend::default();
    backend.set_model_caps("gpt-4-vision", vec![Capability::Vision]);
    let resolver = CapabilityResolver::new(Arc::new(backend));

    let req = ResolveRequest {
        api_key_id: "ak1",
        model_alias: "gpt-4-vision",
        header_require: None,
        protocol: ProtocolKind::OpenAi,
        retry_upgrade: None,
    };
    let caps = resolver.resolve(&req).await.unwrap();
    assert!(caps.set.contains(&Capability::Vision));
    assert_eq!(caps.mode, MatchMode::Compatible);
}

#[tokio::test]
async fn resolve_header_forces_extra_cap() {
    let backend = FakeResolveBackend::default();
    backend.set_model_caps("m1", vec![]);
    let resolver = CapabilityResolver::new(Arc::new(backend));

    let header = ["thinking"];
    let req = ResolveRequest {
        api_key_id: "ak1",
        model_alias: "m1",
        header_require: Some(&header),
        protocol: ProtocolKind::OpenAi,
        retry_upgrade: None,
    };
    let caps = resolver.resolve(&req).await.unwrap();
    assert!(caps.set.contains(&Capability::Thinking));
}

#[tokio::test]
async fn resolve_key_exclusive_sets_mode() {
    let backend = FakeResolveBackend::default();
    backend.set_key_forced("ak1", vec![Capability::Vision]);
    backend.set_match_mode_exclusive("ak1", true);
    let resolver = CapabilityResolver::new(Arc::new(backend));

    let req = ResolveRequest {
        api_key_id: "ak1",
        model_alias: "m1",
        header_require: None,
        protocol: ProtocolKind::OpenAi,
        retry_upgrade: None,
    };
    let caps = resolver.resolve(&req).await.unwrap();
    assert_eq!(caps.mode, MatchMode::Exclusive);
    assert!(caps.set.contains(&Capability::Vision));
}

// ----------------------------------------------------------------------------
// Candidate-filter layer
// ----------------------------------------------------------------------------

fn seed_key(id: &str, forced: &[&str], exclusive: bool) -> VendorKey {
    VendorKey {
        key_id: KeyId::from(id),
        vendor_id: VendorId::from("v1"),
        api_key: SecretString::from("sk".to_string()),
        accessible_models: vec![ModelName::from("m1")],
        model_mapping: Default::default(),
        accessible_endpoints: vec![ProtocolKind::OpenAi],
        protocol_conversion: false,
        priority: 0,
        weight: 1,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: KeyStatus::Active,
        tags: vec![],
        forced_capabilities: forced.iter().map(|s| s.to_string()).collect(),
        exclusive_capability_match: exclusive,
    }
}

fn perm() -> MergedPermissions {
    MergedPermissions {
        user_id: UserId::from("u"),
        effective_user_id: UserId::from("u"),
        client_ip: IpAddr::from([127, 0, 0, 1]),
        allowed_models: HashSet::new(),
        allowed_channels: HashSet::new(),
        allowed_groups: HashSet::new(),
        forced_model_group: None,
        rpm_limit: 0,
        tpm_limit: 0,
        max_concurrent_streams: 0,
        markup_chain: Default::default(),
    }
}

fn group_with(ids: &[&str]) -> ModelGroup {
    ModelGroup {
        group: Group::from("g1"),
        strategy: StrategyKind::FixedOrder,
        cross_vendor_fallback: false,
        max_tokens_cap: None,
        default_params: Default::default(),
        forced_params: Default::default(),
        vendor_key_ids: ids.iter().map(|s| KeyId::from(*s)).collect(),
    }
}

async fn store_with(keys: Vec<VendorKey>) -> Arc<EntityStore> {
    let store = Arc::new(EntityStore::new());
    for k in keys {
        let id = k.key_id.clone();
        store
            .vendor_key
            .insert(id, Arc::new(Versioned::new(1, k)))
            .await;
    }
    store
}

fn req_caps(set: &[Capability], mode: MatchMode) -> gw_capability::types::RequiredCapabilities {
    let mut r = gw_capability::types::RequiredCapabilities::default();
    for c in set {
        r.add(*c, gw_capability::types::CapabilityOrigin::Model);
    }
    r.mode = mode;
    r
}

/// Vision requirement: only the Vision-advertising key and the unadvertised
/// (non-restrictive) key may route; the audio-only key is dropped.
#[tokio::test]
async fn vision_required_filters_non_vision_keys() {
    let store = store_with(vec![
        seed_key("k_vision", &["vision"], false),
        seed_key("k_audio", &["audio"], false),
        seed_key("k_open", &[], false),
    ])
    .await;
    let group = group_with(&["k_vision", "k_audio", "k_open"]);
    let in_flight = InFlightStore::default();
    let caps = req_caps(&[Capability::Vision], MatchMode::Compatible);

    let out = get_candidate_keys(
        &group,
        &ModelName::from("m1"),
        ProtocolKind::OpenAi,
        &perm(),
        &in_flight,
        &store,
        1024,
        &[],
        Some(&caps),
    )
    .await;
    let ids: Vec<&str> = out.iter().map(|k| k.key_id.as_ref()).collect();
    assert!(ids.contains(&"k_vision"));
    assert!(ids.contains(&"k_open"));
    assert!(!ids.contains(&"k_audio"));
}

/// `None` required caps disables the filter entirely (v1 unconstrained route).
#[tokio::test]
async fn none_caps_admits_all() {
    let store = store_with(vec![
        seed_key("k_vision", &["vision"], false),
        seed_key("k_audio", &["audio"], false),
    ])
    .await;
    let group = group_with(&["k_vision", "k_audio"]);
    let in_flight = InFlightStore::default();

    let out = get_candidate_keys(
        &group,
        &ModelName::from("m1"),
        ProtocolKind::OpenAi,
        &perm(),
        &in_flight,
        &store,
        1024,
        &[],
        None,
    )
    .await;
    assert_eq!(out.len(), 2);
}

/// Exclusive channel with `forced_capabilities = [vision]` only matches a
/// requirement set of *exactly* `[vision]`; a `[vision, thinking]` request is
/// rejected even though vision is satisfied.
#[tokio::test]
async fn exclusive_channel_requires_exact_match() {
    let store = store_with(vec![seed_key("k_excl", &["vision"], true)]).await;
    let group = group_with(&["k_excl"]);
    let in_flight = InFlightStore::default();

    // Exact match → admitted.
    let exact = req_caps(&[Capability::Vision], MatchMode::Compatible);
    let out = get_candidate_keys(
        &group,
        &ModelName::from("m1"),
        ProtocolKind::OpenAi,
        &perm(),
        &in_flight,
        &store,
        1024,
        &[],
        Some(&exact),
    )
    .await;
    assert_eq!(out.len(), 1);

    // Superset request → rejected (channel advertises only vision, but the
    // request also needs thinking → vision channel cannot fully satisfy).
    let superset = req_caps(
        &[Capability::Vision, Capability::Thinking],
        MatchMode::Compatible,
    );
    let out = get_candidate_keys(
        &group,
        &ModelName::from("m1"),
        ProtocolKind::OpenAi,
        &perm(),
        &in_flight,
        &store,
        1024,
        &[],
        Some(&superset),
    )
    .await;
    assert!(out.is_empty());
}
