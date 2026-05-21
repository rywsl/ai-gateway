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
//! ## Dispatch shape
//!
//! [`Self::pick_for`] returns `&StrategyAny` (not `&dyn Strategy`) so the hot
//! path stays on the codebase's enum-dispatch contract (see
//! `crates/gw-routing/src/strategy/mod.rs` doc on `StrategyAny`: "Avoids
//! heap-allocated trait objects on the hot path"). The set stores one
//! pre-built `StrategyAny` per kind, so dispatch is a borrow + enum match,
//! never a vtable call.
//!
//! ## T2-C migration note
//!
//! When `Sticky` grows a `moka::sync::Cache` and becomes expensive to clone,
//! `StrategyAny::Sticky` will likely change to hold `Arc<Sticky>`. Because we
//! return `&StrategyAny` (a borrow), no by-value moves are baked into this
//! API — that future change stays local to `strategy/mod.rs` + this file's
//! constructors and does not ripple into call sites.
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
use crate::strategy::StrategyAny;
use gw_core::entity::StrategyKind;

/// One instance of each of the 8 strategy variants, owned for the lifetime of
/// the process and shared across all requests via `Arc<StrategySet>`.
///
/// Fields are private; access is via [`Self::pick_for`] which enforces the
/// `StrategyKind` → variant mapping and returns `&StrategyAny` so callers
/// stay on enum dispatch.
pub struct StrategySet {
    fixed_order: StrategyAny,
    round_robin: StrategyAny,
    load_balance: StrategyAny,
    weighted_rr: StrategyAny,
    cost_first: StrategyAny,
    latency_first: StrategyAny,
    cache_affinity: StrategyAny,
    /// T2-C migrates this to moka-backed shared state.
    sticky: StrategyAny,
}

impl StrategySet {
    /// Construct with the production defaults for each strategy.
    pub fn new_default() -> Self {
        Self {
            fixed_order: StrategyAny::FixedOrder(FixedOrder),
            round_robin: StrategyAny::RoundRobin(RoundRobin::default()),
            load_balance: StrategyAny::LoadBalance(LoadBalance),
            weighted_rr: StrategyAny::WeightedRr(WeightedRr::default()),
            cost_first: StrategyAny::CostFirst(CostFirst),
            latency_first: StrategyAny::LatencyFirst(LatencyFirst),
            cache_affinity: StrategyAny::CacheAffinity(CacheAffinity),
            sticky: StrategyAny::Sticky(Sticky::default()),
        }
    }

    /// Sentinel constructor for Wave 0 scaffolding. Equivalent to
    /// [`Self::new_default`] today; kept as a separate name so T2-C's moka
    /// migration can replace `new_default` without touching wave-0 use sites.
    pub fn placeholder() -> Self {
        Self::new_default()
    }

    /// Dispatch by [`StrategyKind`] to the owned [`StrategyAny`] instance.
    ///
    /// Returns `&StrategyAny` (not `&dyn Strategy`) to stay on enum dispatch
    /// — same shape that the legacy `strategy_for(kind) -> StrategyAny`
    /// factory in `gw-proxy/src/filters/upstream.rs` produces.
    pub fn pick_for(&self, kind: StrategyKind) -> &StrategyAny {
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
