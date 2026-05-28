use gw_core::entity::*;
use gw_core::ids::*;
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_routing::latency::LatencyTracker;
use gw_routing::pipeline::{self, DimMask, ScoringPolicy};
use gw_routing::strategy::*;
use secrecy::SecretString;
use std::sync::Arc;
use std::time::Duration;

fn fake_key(id: &str, priority: i32, weight: u32) -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from(id),
        vendor_id: VendorId::from("v1"),
        api_key: SecretString::from("sk".to_string()),
        accessible_models: vec![],
        model_mapping: Default::default(),
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority,
        weight,
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
    })
}

fn ctx<'a>(uid: &'a UserId, model: &'a ModelName, lt: &'a LatencyTracker) -> PickCtx<'a> {
    PickCtx {
        user_id: uid,
        model,
        conversation_id: Some("conv-a"),
        message_hash: Some(0xabc),
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
        pricing: None,
        latency_tracker: lt,
        reservation_view: None,
        frequency_tracker: None,
    }
}

#[test]
fn fixed_order_pipeline_matches_strategy() {
    let keys = vec![
        fake_key("a", 1, 1),
        fake_key("b", 10, 1),
        fake_key("c", 5, 1),
    ];
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let uid = UserId::from("u");
    let model = ModelName::from("m");
    let ctx = ctx(&uid, &model, &lt);
    let s = fixed_order::FixedOrder;
    let legacy = s.pick(&keys, &h, &f, &ctx).unwrap().key_id.clone();
    let piped = pipeline::execute(&s, &keys, &h, &f, &ctx)
        .unwrap()
        .key_id
        .clone();
    assert_eq!(legacy, piped);
}

#[test]
fn cost_first_pipeline_matches_strategy() {
    let keys = vec![
        fake_key("a", -5, 1),
        fake_key("b", 7, 1),
        fake_key("c", 3, 1),
    ];
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    let uid = UserId::from("u");
    let model = ModelName::from("m");
    let ctx = ctx(&uid, &model, &lt);
    let s = cost_first::CostFirst;
    let legacy = s.pick(&keys, &h, &f, &ctx).unwrap().key_id.clone();
    let piped = pipeline::execute(&s, &keys, &h, &f, &ctx)
        .unwrap()
        .key_id
        .clone();
    assert_eq!(legacy, piped);
}

#[test]
fn latency_first_pipeline_matches_strategy() {
    let keys = vec![
        fake_key("a", 1, 1),
        fake_key("b", 10, 1),
        fake_key("c", 5, 1),
    ];
    let h = KeyHealthStore::default();
    let f = InFlightStore::default();
    let lt = LatencyTracker::new();
    for _ in 0..10 {
        lt.record_ttft(
            &keys[0].key_id,
            gw_core::protocol::ProtocolKind::OpenAi,
            Duration::from_millis(200),
        );
        lt.record_ttft(
            &keys[1].key_id,
            gw_core::protocol::ProtocolKind::OpenAi,
            Duration::from_millis(50),
        );
    }
    let uid = UserId::from("u");
    let model = ModelName::from("m");
    let ctx = ctx(&uid, &model, &lt);
    let s = latency_first::LatencyFirst;
    let legacy = s.pick(&keys, &h, &f, &ctx).unwrap().key_id.clone();
    let piped = pipeline::execute(&s, &keys, &h, &f, &ctx)
        .unwrap()
        .key_id
        .clone();
    assert_eq!(legacy, piped);
}

#[test]
fn strategy_dims_are_minimal() {
    let lt = LatencyTracker::new();
    let uid = UserId::from("u");
    let model = ModelName::from("m");
    let ctx = ctx(&uid, &model, &lt);
    assert_eq!(
        fixed_order::FixedOrder.needed_dims(&ctx),
        DimMask::HEALTH_ONLY
    );
    assert_eq!(
        round_robin::RoundRobin::new().needed_dims(&ctx),
        DimMask::HEALTH_ONLY
    );
    assert!(cost_first::CostFirst
        .needed_dims(&ctx)
        .contains(DimMask::HEALTH));
    assert!(latency_first::LatencyFirst
        .needed_dims(&ctx)
        .contains(DimMask::LATENCY));
    assert!(load_balance::LoadBalance
        .needed_dims(&ctx)
        .contains(DimMask::INFLIGHT));
}
