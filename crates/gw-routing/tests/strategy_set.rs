//! T0-B: StrategySet container — one stateful instance per strategy kind,
//! shared across requests.

use gw_core::entity::StrategyKind;
use gw_routing::strategy::set::StrategySet;
use gw_routing::strategy::Strategy;

/// Expected `Strategy::name()` for each `StrategyKind`. A mis-wired arm in
/// `StrategySet::pick_for` (e.g. `Sticky => &self.round_robin`) would fail
/// this — the previous `!name().is_empty()` assertion would not.
fn expected_name(kind: StrategyKind) -> &'static str {
    match kind {
        StrategyKind::FixedOrder => "fixed_order",
        StrategyKind::RoundRobin => "round_robin",
        StrategyKind::LoadBalance => "load_balance",
        StrategyKind::WeightedRr => "weighted_rr",
        StrategyKind::CostFirst => "cost_first",
        StrategyKind::LatencyFirst => "latency_first",
        StrategyKind::CacheAffinity => "cache_affinity",
        StrategyKind::Sticky => "sticky",
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
        let s = set.pick_for(kind);
        assert_eq!(
            s.name(),
            expected_name(kind),
            "strategy {:?} dispatched to wrong variant: got {}",
            kind,
            s.name(),
        );
    }
}

#[test]
fn placeholder_equivalent_to_default() {
    let _a = StrategySet::placeholder();
    let _b = StrategySet::new_default();
    // Both compile and are usable; placeholder is a named alias today so that
    // T2-C's moka migration can replace `new_default` without touching wave-0
    // use sites.
}
