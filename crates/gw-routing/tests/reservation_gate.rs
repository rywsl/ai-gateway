use gw_core::entity::{KeyStatus, VendorKey};
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::protocol::ProtocolKind;
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_routing::latency::LatencyTracker;
use gw_routing::pipeline::multi_dim::DimWeights;
use gw_routing::reservation::{ReservationEvent, ReservationGlobalView};
use gw_routing::reservation_gate::ReservationGate;
use gw_routing::strategy::multi_dim::MultiDim;
use gw_routing::strategy::{PickCtx, Strategy};
use secrecy::SecretString;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

fn fake_key(id: &str, priority: i32) -> Arc<VendorKey> {
    Arc::new(VendorKey {
        key_id: KeyId::from(id),
        vendor_id: VendorId::from("v1"),
        api_key: SecretString::from("sk".to_string()),
        accessible_models: vec![],
        model_mapping: Default::default(),
        accessible_endpoints: vec![],
        protocol_conversion: false,
        priority,
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

fn hot_view(model: &str, rpm: f64) -> ReservationGlobalView {
    let view = ReservationGlobalView::new();
    view.apply_event(ReservationEvent {
        instance_id: "instance-a".to_string(),
        model: ModelName::from(model),
        rate: rpm / 60.0,
        ts_ms: 1_000,
    });
    view
}

fn ctx<'a>(
    user: &'a UserId,
    model: &'a ModelName,
    latency: &'a LatencyTracker,
    view: Option<&'a ReservationGlobalView>,
) -> PickCtx<'a> {
    PickCtx {
        user_id: user,
        model,
        conversation_id: None,
        message_hash: None,
        api_format: ProtocolKind::OpenAi,
        pricing: None,
        latency_tracker: latency,
        reservation_view: view,
        frequency_tracker: None,
    }
}

#[test]
fn disabled_gate_passes_even_when_over_threshold() {
    let model = ModelName::from("gpt-4o");
    let view = hot_view("gpt-4o", 120.0);
    let gate = ReservationGate::disabled(HashMap::from([("gpt-*".to_string(), 60)]));

    assert!(gate.allows(&model, Some(&view)));
}

#[test]
fn over_threshold_model_is_filtered() {
    let model = ModelName::from("gpt-4o");
    let view = hot_view("gpt-4o", 61.0);
    let gate = ReservationGate::enabled(HashMap::from([("gpt-*".to_string(), 60)]));

    assert!(!gate.allows(&model, Some(&view)));
}

#[test]
fn under_threshold_model_passes() {
    let model = ModelName::from("gpt-4o");
    let view = hot_view("gpt-4o", 60.0);
    let gate = ReservationGate::enabled(HashMap::from([("gpt-*".to_string(), 60)]));

    assert!(gate.allows(&model, Some(&view)));
}

#[test]
fn no_threshold_for_model_passes() {
    let model = ModelName::from("claude-3-5-sonnet");
    let view = hot_view("claude-3-5-sonnet", 10_000.0);
    let gate = ReservationGate::enabled(HashMap::from([("gpt-*".to_string(), 60)]));

    assert!(gate.allows(&model, Some(&view)));
}

#[test]
fn enabled_gate_rejects_cold_start_without_view_or_grace() {
    let model = ModelName::from("gpt-4o");
    let gate = ReservationGate::enabled(HashMap::from([("gpt-*".to_string(), 60)]));

    assert!(!gate.allows(&model, None));
}

#[test]
fn enabled_gate_accepts_after_startup_grace_without_view() {
    let model = ModelName::from("gpt-4o");
    let gate = ReservationGate::with_startup_grace(
        true,
        HashMap::from([("gpt-*".to_string(), 60)]),
        Duration::ZERO,
    );

    assert!(gate.allows(&model, None));
}

#[test]
fn enabled_gate_accepts_after_first_reservation_event_even_during_grace() {
    let model = ModelName::from("gpt-4o");
    let gate = ReservationGate::enabled(HashMap::from([("gpt-*".to_string(), 60)]));
    let view = hot_view("gpt-4o", 10.0);

    assert!(gate.allows(&model, Some(&view)));
}

#[test]
fn pick_flow_excludes_all_candidates_for_hot_model_before_scoring() {
    let user = UserId::from("u1");
    let model = ModelName::from("gpt-4o");
    let latency = LatencyTracker::new();
    let view = ReservationGlobalView::with_gate(ReservationGate::enabled(HashMap::from([(
        "gpt-*".to_string(),
        60,
    )])));
    view.apply_event(ReservationEvent {
        instance_id: "instance-a".to_string(),
        model: ModelName::from("gpt-4o"),
        rate: 120.0 / 60.0,
        ts_ms: 1_000,
    });
    let pick_ctx = ctx(&user, &model, &latency, Some(&view));
    let strategy = MultiDim::new("balanced", DimWeights::raw(1.0, 0.0, 0.0, 0.0, 0.0));
    let candidates = vec![fake_key("k1", 10), fake_key("k2", 5)];

    let picked = strategy.pick(
        &candidates,
        &KeyHealthStore::default(),
        &InFlightStore::default(),
        &pick_ctx,
    );

    assert!(picked.is_none());
}

#[test]
fn pick_flow_keeps_scoring_for_models_without_threshold() {
    let user = UserId::from("u1");
    let model = ModelName::from("claude-3-5-sonnet");
    let latency = LatencyTracker::new();
    let view = ReservationGlobalView::with_gate(ReservationGate::enabled(HashMap::from([(
        "gpt-*".to_string(),
        60,
    )])));
    view.apply_event(ReservationEvent {
        instance_id: "instance-a".to_string(),
        model: ModelName::from("claude-3-5-sonnet"),
        rate: 10_000.0 / 60.0,
        ts_ms: 1_000,
    });
    let pick_ctx = ctx(&user, &model, &latency, Some(&view));
    let strategy = MultiDim::new("balanced", DimWeights::raw(1.0, 0.0, 0.0, 0.0, 0.0));
    let candidates = vec![fake_key("k1", 10), fake_key("k2", 5)];

    let picked = strategy
        .pick(
            &candidates,
            &KeyHealthStore::default(),
            &InFlightStore::default(),
            &pick_ctx,
        )
        .expect("candidate selected");

    assert_eq!(picked.key_id, KeyId::from("k1"));
}
