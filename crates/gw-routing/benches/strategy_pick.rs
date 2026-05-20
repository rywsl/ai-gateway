//! Routing strategy `pick` microbenchmark.
//!
//! Benches each of the eight `Strategy` impls over an 8-key candidate slice
//! with all keys healthy (the happy-path). The slice is held by `Arc` so
//! the per-iteration overhead is just the strategy's bookkeeping (sort,
//! atomic counter, hash, etc.). We use a global `KeyHealthStore` (all
//! entries created healthy) and `InFlightStore` pre-warmed with empty
//! entries so insertion cost is not included in the hot path.
//!
//! Spec target: no single strategy P99 > 100 µs at slice-size 8.
//!
//! Run with: `cargo bench -p gw-routing --bench strategy_pick`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gw_core::entity::*;
use gw_core::ids::*;
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_routing::strategy::*;
use secrecy::SecretString;
use std::sync::Arc;

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
    })
}

fn candidates_eight() -> Vec<Arc<VendorKey>> {
    (0..8_i32)
        .map(|i| fake_key(&format!("k{i}"), 8 - i, 1 + (i as u32 % 3)))
        .collect()
}

fn bench_strategy_pick(c: &mut Criterion) {
    let keys = candidates_eight();
    let health = KeyHealthStore::default();
    let in_flight = InFlightStore::default();
    // Pre-warm both stores so DashMap insertion cost is not in the hot path.
    // The old single-store BreakerStore covered both concerns in one lookup;
    // post-split we touch each store explicitly.
    for k in keys.iter() {
        let _ = in_flight.get_or_create(&k.key_id);
        // Warms `KeyHealthStore::by_key_format` via the public read path.
        let _ = health.is_available(&k.key_id, gw_core::protocol::ProtocolKind::OpenAi);
    }
    let uid = UserId::from("u-bench");
    let model = ModelName::from("gpt-4");
    let conv = "conv-1";

    let ctx_no_hash = PickCtx {
        user_id: &uid,
        model: &model,
        conversation_id: None,
        message_hash: None,
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
    };
    let ctx_with_hash = PickCtx {
        user_id: &uid,
        model: &model,
        conversation_id: Some(conv),
        message_hash: Some(0xdead_beef_dead_beef),
        api_format: gw_core::protocol::ProtocolKind::OpenAi,
    };

    use gw_routing::strategy::{
        cache_affinity::CacheAffinity, cost_first::CostFirst, fixed_order::FixedOrder,
        latency_first::LatencyFirst, load_balance::LoadBalance, round_robin::RoundRobin,
        sticky::Sticky, weighted_rr::WeightedRr,
    };

    c.bench_function("fixed_order_pick_8", |b| {
        let s = FixedOrder;
        b.iter(|| {
            let picked = s.pick(
                black_box(&keys),
                black_box(&health),
                black_box(&in_flight),
                black_box(&ctx_no_hash),
            );
            black_box(picked);
        });
    });

    c.bench_function("round_robin_pick_8", |b| {
        let s = RoundRobin::new();
        b.iter(|| {
            let picked = s.pick(
                black_box(&keys),
                black_box(&health),
                black_box(&in_flight),
                black_box(&ctx_no_hash),
            );
            black_box(picked);
        });
    });

    c.bench_function("load_balance_pick_8", |b| {
        let s = LoadBalance;
        b.iter(|| {
            let picked = s.pick(
                black_box(&keys),
                black_box(&health),
                black_box(&in_flight),
                black_box(&ctx_no_hash),
            );
            black_box(picked);
        });
    });

    c.bench_function("weighted_rr_pick_8", |b| {
        let s = WeightedRr::new();
        b.iter(|| {
            let picked = s.pick(
                black_box(&keys),
                black_box(&health),
                black_box(&in_flight),
                black_box(&ctx_no_hash),
            );
            black_box(picked);
        });
    });

    c.bench_function("cost_first_pick_8", |b| {
        let s = CostFirst;
        b.iter(|| {
            let picked = s.pick(
                black_box(&keys),
                black_box(&health),
                black_box(&in_flight),
                black_box(&ctx_no_hash),
            );
            black_box(picked);
        });
    });

    c.bench_function("latency_first_pick_8", |b| {
        let s = LatencyFirst;
        b.iter(|| {
            let picked = s.pick(
                black_box(&keys),
                black_box(&health),
                black_box(&in_flight),
                black_box(&ctx_no_hash),
            );
            black_box(picked);
        });
    });

    c.bench_function("cache_affinity_pick_8", |b| {
        let s = CacheAffinity;
        b.iter(|| {
            let picked = s.pick(
                black_box(&keys),
                black_box(&health),
                black_box(&in_flight),
                black_box(&ctx_with_hash),
            );
            black_box(picked);
        });
    });

    c.bench_function("sticky_pick_8", |b| {
        let s = Sticky::default();
        b.iter(|| {
            let picked = s.pick(
                black_box(&keys),
                black_box(&health),
                black_box(&in_flight),
                black_box(&ctx_with_hash),
            );
            black_box(picked);
        });
    });
}

criterion_group!(benches, bench_strategy_pick);
criterion_main!(benches);
