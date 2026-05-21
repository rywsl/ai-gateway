//! StrategySet — per-process container of stateful strategy instances.
//!
//! Stateful strategies (today: [`Sticky`]; later: `LatencyFirst` with EWMA
//! tracking, etc.) must be instantiated once per process and shared across
//! requests. Stateless strategies are kept here too so dispatch is uniform.
//!
//! Replaces the per-request `strategy_for(kind)` factory in
//! `gw-proxy/src/filters/upstream.rs`, which constructed a fresh
//! `Sticky::default()` every call and therefore leaked all session affinity.
//!
//! Spec ADR refs: G12.

use crate::strategy::cache_affinity::CacheAffinity;
use crate::strategy::cost_first::CostFirst;
use crate::strategy::fixed_order::FixedOrder;
use crate::strategy::latency_first::LatencyFirst;
use crate::strategy::load_balance::LoadBalance;
use crate::strategy::round_robin::RoundRobin;
use crate::strategy::sticky::Sticky;
use crate::strategy::weighted_rr::WeightedRr;
use crate::strategy::Strategy;
use gw_core::entity::StrategyKind;

/// One instance of each of the 8 strategy variants, owned for the lifetime of
/// the process and shared across all requests via `Arc<StrategySet>`.
pub struct StrategySet {
    pub fixed_order: FixedOrder,
    pub round_robin: RoundRobin,
    pub load_balance: LoadBalance,
    pub weighted_rr: WeightedRr,
    pub cost_first: CostFirst,
    pub latency_first: LatencyFirst,
    pub cache_affinity: CacheAffinity,
    /// T2-C migrates this to moka-backed shared state.
    pub sticky: Sticky,
}

impl StrategySet {
    /// Construct with the production defaults for each strategy.
    pub fn new_default() -> Self {
        Self {
            fixed_order: FixedOrder,
            round_robin: RoundRobin::default(),
            load_balance: LoadBalance,
            weighted_rr: WeightedRr::default(),
            cost_first: CostFirst,
            latency_first: LatencyFirst,
            cache_affinity: CacheAffinity,
            sticky: Sticky::default(),
        }
    }

    /// Sentinel constructor for Wave 0 scaffolding. Equivalent to
    /// [`Self::new_default`] today; kept as a separate name so T2-C's moka
    /// migration can replace `new_default` without touching wave-0 use sites.
    pub fn placeholder() -> Self {
        Self::new_default()
    }

    /// Dispatch by [`StrategyKind`] to the owned instance.
    pub fn pick_for(&self, kind: StrategyKind) -> &dyn Strategy {
        match kind {
            StrategyKind::FixedOrder => &self.fixed_order,
            StrategyKind::RoundRobin => &self.round_robin,
            StrategyKind::LoadBalance => &self.load_balance,
            StrategyKind::WeightedRr => &self.weighted_rr,
            StrategyKind::CostFirst => &self.cost_first,
            StrategyKind::LatencyFirst => &self.latency_first,
            StrategyKind::CacheAffinity => &self.cache_affinity,
            StrategyKind::Sticky => &self.sticky,
        }
    }
}
