//! Unit tests for selection strategies.

use gw_core::entity::*;
use gw_core::ids::*;
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_routing::latency::LatencyTracker;
use gw_routing::strategy::*;
use secrecy::SecretString;
use std::collections::HashMap;
use std::sync::Arc;

fn fake_key(id: &str, prio: i32) -> Arc<VendorKey> {
    fake_key_with_cost(id, prio, "", None)
}

fn fake_key_with_cost(
    id: &str,
    prio: i32,
    model: &str,
    cost_weight: Option<u32>,
) -> Arc<VendorKey> {
    let model_cost_weights = cost_weight
        .map(|weight| HashMap::from([(ModelName::from(model), weight)]))
        .unwrap_or_default();

    Arc::new(VendorKey {
        key_id: KeyId::from(id),
        vendor_id: VendorId::from("v1"),
        api_key: SecretString::from("sk".to_string()),
        accessible_models: vec![],
        model_mapping: Default::default(),
        model_cost_weights,
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
        identity_snapshot: None,
        quota_state: Default::default(),
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

#[test]
fn cost_first_uses_provider_model_cost_weight_when_pricing_exists() {
    use gw_routing::strategy::cost_first::CostFirst;

    let model = ModelName::from("gpt-4o");
    let pricing = Pricing {
        model: model.clone(),
        input_price_micro_per_token: 1,
        output_price_micro_per_token: 1,
        max_output: 4096,
        model_markup: 1.0,
        connect_timeout_ms: None,
        read_timeout_ms: None,
        total_timeout_ms: None,
        stream_total_timeout_ms: None,
    };
    let keys = vec![
        fake_key_with_cost("expensive", 1, "gpt-4o", Some(1500)),
        fake_key_with_cost("cheap", 100, "gpt-4o", Some(700)),
    ];
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &model,
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
        pricing: Some(&pricing),
        latency_tracker: &lt,
        reservation_view: None,
        frequency_tracker: None,
    };

    let picked = CostFirst.pick(&keys, &h, &f, &ctx).unwrap();
    assert_eq!(picked.key_id.as_ref(), "cheap");
}

#[test]
fn cost_first_missing_provider_model_cost_weight_uses_neutral_1000() {
    use gw_routing::strategy::cost_first::CostFirst;

    let model = ModelName::from("gpt-4o");
    let pricing = Pricing {
        model: model.clone(),
        input_price_micro_per_token: 1,
        output_price_micro_per_token: 1,
        max_output: 4096,
        model_markup: 1.0,
        connect_timeout_ms: None,
        read_timeout_ms: None,
        total_timeout_ms: None,
        stream_total_timeout_ms: None,
    };
    let keys = vec![
        fake_key_with_cost("missing", 1, "other-model", Some(1)),
        fake_key_with_cost("explicit-cheap", 100, "gpt-4o", Some(900)),
    ];
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let ctx = PickCtx {
        user_id: &UserId::from("u"),
        model: &model,
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
        pricing: Some(&pricing),
        latency_tracker: &lt,
        reservation_view: None,
        frequency_tracker: None,
    };

    let picked = CostFirst.pick(&keys, &h, &f, &ctx).unwrap();
    assert_eq!(picked.key_id.as_ref(), "explicit-cheap");
}
