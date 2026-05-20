//! Unit tests for selection strategies.

use gw_core::entity::*;
use gw_core::ids::*;
use gw_routing::breaker::BreakerStore;
use gw_routing::strategy::*;
use secrecy::SecretString;
use std::sync::Arc;

fn fake_key(id: &str, prio: i32) -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from(id),
        vendor_id: VendorId::from("v1"),
        api_key: SecretString::from("sk".to_string()),
        accessible_models: vec![],
        model_mapping: Default::default(),
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority: prio,
        weight: 1,
        rpm_limit: 0,
        tpm_limit: 0,
        max_usage_count: None,
        proxy: None,
        ip_allowlist: None,
        expires_at: None,
        status: KeyStatus::Active,
        tags: vec![],
    })
}

#[test]
fn fixed_order_picks_highest_priority() {
    use gw_routing::strategy::fixed_order::FixedOrder;
    let keys = vec![fake_key("a", 0), fake_key("b", 10), fake_key("c", 5)];
    let b = BreakerStore::default();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &ModelName::from("m"),
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
    };
    let picked = FixedOrder.pick(&keys, &b, &ctx).unwrap();
    assert_eq!(picked.key_id.as_ref(), "b");
}

#[test]
fn round_robin_rotates_through_three_keys() {
    use gw_routing::strategy::round_robin::RoundRobin;
    let keys = vec![fake_key("a", 0), fake_key("b", 0), fake_key("c", 0)];
    let b = BreakerStore::default();
    let rr = RoundRobin::new();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &ModelName::from("m"),
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
    };
    let p1 = rr.pick(&keys, &b, &ctx).unwrap().key_id.clone();
    let p2 = rr.pick(&keys, &b, &ctx).unwrap().key_id.clone();
    let p3 = rr.pick(&keys, &b, &ctx).unwrap().key_id.clone();
    let mut set = std::collections::HashSet::new();
    set.insert(p1);
    set.insert(p2);
    set.insert(p3);
    assert_eq!(set.len(), 3, "round-robin should hit all three keys");
}

#[test]
fn cost_first_picks_highest_priority_proxy() {
    use gw_routing::strategy::cost_first::CostFirst;
    let keys = vec![fake_key("a", -10), fake_key("b", 50), fake_key("c", 10)];
    let b = BreakerStore::default();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &ModelName::from("m"),
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
    };
    let picked = CostFirst.pick(&keys, &b, &ctx).unwrap();
    assert_eq!(picked.key_id.as_ref(), "b");
}
