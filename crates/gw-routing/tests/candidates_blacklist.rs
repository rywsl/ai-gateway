//! T3-B (G10) — `get_candidate_keys` must drop any `KeyId` listed in the
//! cross-key retry blacklist. The blacklist is empty on the first attempt
//! and grows by one each time `fail_to_connect` decides to retry.

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

fn seed_key(id: &str) -> VendorKey {
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
        forced_capabilities: vec![],
        exclusive_capability_match: false,
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

async fn store_with(keys: &[&str]) -> Arc<EntityStore> {
    let store = Arc::new(EntityStore::new());
    for k in keys {
        store
            .vendor_key
            .insert(KeyId::from(*k), Arc::new(Versioned::new(1, seed_key(k))))
            .await;
    }
    store
}

#[tokio::test]
async fn empty_blacklist_no_op() {
    let store = store_with(&["k1", "k2", "k3"]).await;
    let group = group_with(&["k1", "k2", "k3"]);
    let perm = perm();
    let in_flight = InFlightStore::default();
    let out = get_candidate_keys(
        &group,
        &ModelName::from("m1"),
        ProtocolKind::OpenAi,
        &perm,
        &in_flight,
        &store,
        1024,
        &[],
    )
    .await;
    let ids: Vec<&str> = out.iter().map(|k| k.key_id.as_ref()).collect();
    assert_eq!(ids, vec!["k1", "k2", "k3"]);
}

#[tokio::test]
async fn blacklisted_key_filtered() {
    let store = store_with(&["k1", "k2", "k3"]).await;
    let group = group_with(&["k1", "k2", "k3"]);
    let perm = perm();
    let in_flight = InFlightStore::default();
    let blacklist = vec![KeyId::from("k2")];
    let out = get_candidate_keys(
        &group,
        &ModelName::from("m1"),
        ProtocolKind::OpenAi,
        &perm,
        &in_flight,
        &store,
        1024,
        &blacklist,
    )
    .await;
    let ids: Vec<&str> = out.iter().map(|k| k.key_id.as_ref()).collect();
    assert_eq!(ids, vec!["k1", "k3"]);
}

#[tokio::test]
async fn all_blacklisted_returns_empty() {
    let store = store_with(&["k1", "k2"]).await;
    let group = group_with(&["k1", "k2"]);
    let perm = perm();
    let in_flight = InFlightStore::default();
    let blacklist = vec![KeyId::from("k1"), KeyId::from("k2")];
    let out = get_candidate_keys(
        &group,
        &ModelName::from("m1"),
        ProtocolKind::OpenAi,
        &perm,
        &in_flight,
        &store,
        1024,
        &blacklist,
    )
    .await;
    assert!(out.is_empty());
}
