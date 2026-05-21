//! T0-B: StrategySet container — one stateful instance per strategy kind,
//! shared across requests.

use gw_core::entity::StrategyKind;
use gw_routing::strategy::set::StrategySet;

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
        assert!(!s.name().is_empty(), "strategy {:?} returns name", kind);
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
