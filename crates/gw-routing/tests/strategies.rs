//! Unit tests for selection strategies.

use gw_core::entity::*;
use gw_core::ids::*;
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_routing::latency::LatencyTracker;
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
        forced_capabilities: vec![],
        exclusive_capability_match: false,
        tunnel_preference: None,
        oauth: None,
    })
}

#[test]
fn fixed_order_picks_highest_priority() {
    use gw_routing::strategy::fixed_order::FixedOrder;
    let keys = vec![fake_key("a", 0), fake_key("b", 10), fake_key("c", 5)];
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &ModelName::from("m"),
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
        pricing: None,
        latency_tracker: &lt,
        reservation_view: None,
        frequency_tracker: None,
    };
    let picked = FixedOrder.pick(&keys, &h, &f, &ctx).unwrap();
    assert_eq!(picked.key_id.as_ref(), "b");
}

#[test]
fn round_robin_rotates_through_three_keys() {
    use gw_routing::strategy::round_robin::RoundRobin;
    let keys = vec![fake_key("a", 0), fake_key("b", 0), fake_key("c", 0)];
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let rr = RoundRobin::new();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &ModelName::from("m"),
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
        pricing: None,
        latency_tracker: &lt,
        reservation_view: None,
        frequency_tracker: None,
    };
    let p1 = rr.pick(&keys, &h, &f, &ctx).unwrap().key_id.clone();
    let p2 = rr.pick(&keys, &h, &f, &ctx).unwrap().key_id.clone();
    let p3 = rr.pick(&keys, &h, &f, &ctx).unwrap().key_id.clone();
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
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &ModelName::from("m"),
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
        pricing: None,
        latency_tracker: &lt,
        reservation_view: None,
        frequency_tracker: None,
    };
    let picked = CostFirst.pick(&keys, &h, &f, &ctx).unwrap();
    assert_eq!(picked.key_id.as_ref(), "b");
}
