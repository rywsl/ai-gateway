//! T0-B: StrategySet container — one stateful instance per strategy kind,
//! shared across requests.

use gw_core::entity::StrategyKind;
use gw_routing::pipeline::multi_dim::DimWeights;
use gw_routing::strategy::set::StrategySet;
use gw_routing::strategy::Strategy;
use std::collections::HashMap;
use std::time::Duration;

/// Expected `Strategy::name()` for each `StrategyKind`. A mis-wired arm in
/// `StrategySet::pick_for` (e.g. `Sticky => &self.round_robin`) would fail
/// this — the previous `!name().is_empty()` assertion would not.
fn expected_name(kind: &StrategyKind) -> &'static str {
    match kind {
        StrategyKind::FixedOrder => "fixed_order",
        StrategyKind::RoundRobin => "round_robin",
        StrategyKind::LoadBalance => "load_balance",
        StrategyKind::WeightedRr => "weighted_rr",
        StrategyKind::CostFirst => "cost_first",
        StrategyKind::LatencyFirst => "latency_first",
        StrategyKind::CacheAffinity => "cache_affinity",
        StrategyKind::Sticky => "sticky",
        StrategyKind::MultiDim(_) => "multi_dim",
    }
}

#[test]
fn strategy_set_dispatches_each_kind() {
    let set = StrategySet::new_default();
    for kind in [
        StrategyKind::FixedOrder,
        StrategyKind::RoundRobin,
        StrategyKind::LoadBalance,
        StrategyKind::WeightedRr,
        StrategyKind::CostFirst,
        StrategyKind::LatencyFirst,
        StrategyKind::CacheAffinity,
        StrategyKind::Sticky,
    ] {
        let s = set.pick_for(&kind);
        assert_eq!(
            s.name(),
            expected_name(&kind),
            "strategy {:?} dispatched to wrong variant: got {}",
            kind,
            s.name(),
        );
    }
}

#[test]
fn strategy_set_dispatches_multi_dim_preset() {
    let presets = HashMap::from([(
        "balanced".to_string(),
        DimWeights::raw(0.25, 0.25, 0.25, 0.15, 0.10),
    )]);
    let set = StrategySet::new_with_presets(Duration::from_secs(1800), presets);
    let strategy = set.pick_for(&StrategyKind::MultiDim("balanced".to_string()));
    assert_eq!(strategy.name(), "multi_dim");
}

#[test]
fn strategy_set_falls_back_unknown_multi_dim_preset() {
    // Audit 2026-05-30 §2.1: an unknown preset name (control-plane skew / typo)
    // must NOT panic the request task. pick_for falls back to the always-present
    // fixed_order strategy (and emits gw_routing_unknown_preset_total + a
    // warn-once) so the request still routes deterministically.
    let set = StrategySet::new_default();
    let strategy = set.pick_for(&StrategyKind::MultiDim("missing".to_string()));
    assert_eq!(
        strategy.name(),
        "fixed_order",
        "unknown multi_dim preset must fall back to fixed_order, not panic"
    );
}

#[test]
fn placeholder_equivalent_to_default() {
    let _a = StrategySet::placeholder();
    let _b = StrategySet::new_default();
    // Both compile and are usable; placeholder is a named alias today so that
    // T2-C's moka migration can replace `new_default` without touching wave-0
    // use sites.
}
