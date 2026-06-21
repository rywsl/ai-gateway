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
//! [`Self::strategy_for_kind`] is the public runtime dispatch surface. It can
//! borrow preconfigured strategies or construct inline strategies. [`Self::pick_for`]
//! only returns borrowed/preconfigured strategies and rejects inline strategy
//! kinds.
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
use crate::strategy::multi_dim::MultiDim;
use crate::strategy::round_robin::RoundRobin;
use crate::strategy::sticky::{Sticky, DEFAULT_STICKY_CAP, DEFAULT_STICKY_TTL_SECS};
use crate::strategy::weighted_rr::WeightedRr;
use crate::strategy::StrategyAny;
use gw_core::entity::StrategyKind;
use gw_core::ids::KeyId;
use moka::sync::Cache;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

const UNKNOWN_PRESETS_WARNED_CAPACITY: u64 = 1000;

/// Presets whose name has already been logged as unknown, so the warn fires
/// once per distinct bad preset rather than once per request (audit 2026-05-30
/// §2.1 / panic→fail-operational). Bounded by `UNKNOWN_PRESETS_WARNED_CAPACITY`
/// via moka::sync::Cache to prevent unbounded growth from misconfigured clients.
static UNKNOWN_PRESETS_WARNED: LazyLock<Cache<String, ()>> = LazyLock::new(|| {
    Cache::builder()
        .max_capacity(UNKNOWN_PRESETS_WARNED_CAPACITY)
        .build()
});

fn dim_weights_from_core(
    weights: gw_core::entity::MultiDimWeights,
) -> crate::pipeline::multi_dim::DimWeights {
    crate::pipeline::multi_dim::DimWeights::raw_with_reservation(
        weights.health,
        weights.latency,
        weights.cost,
        weights.inflight,
        weights.error_rate,
        weights.reservation,
    )
}

pub enum StrategyChoice<'a> {
    Borrowed(&'a StrategyAny),
    Owned(StrategyAny),
}

impl StrategyChoice<'_> {
    pub fn as_strategy(&self) -> &StrategyAny {
        match self {
            StrategyChoice::Borrowed(strategy) => strategy,
            StrategyChoice::Owned(strategy) => strategy,
        }
    }
}

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
    multi_dim: HashMap<String, StrategyAny>,
}

impl StrategySet {
    /// Construct the 8 strategy variants with their production defaults,
    /// parameterised on `sticky_ttl` so deployments can override the
    /// `Sticky` binding TTL via `routing.sticky_ttl_secs` (ADR-S4).
    ///
    /// All other strategies use their existing defaults — only `Sticky`
    /// reads a configurable knob in v1.
    pub fn new(sticky_ttl: Duration) -> Self {
        Self::new_with_presets(sticky_ttl, HashMap::new())
    }

    pub fn new_with_presets(
        sticky_ttl: Duration,
        presets: HashMap<String, crate::pipeline::multi_dim::DimWeights>,
    ) -> Self {
        let multi_dim = presets
            .into_iter()
            .map(|(name, weights)| {
                let strategy_name = name.clone();
                (
                    name,
                    StrategyAny::MultiDim(MultiDim::new(strategy_name, weights)),
                )
            })
            .collect();
        Self {
            fixed_order: StrategyAny::FixedOrder(FixedOrder),
            round_robin: StrategyAny::RoundRobin(RoundRobin::default()),
            load_balance: StrategyAny::LoadBalance(LoadBalance),
            weighted_rr: StrategyAny::WeightedRr(WeightedRr::default()),
            cost_first: StrategyAny::CostFirst(CostFirst),
            latency_first: StrategyAny::LatencyFirst(LatencyFirst),
            cache_affinity: StrategyAny::CacheAffinity(CacheAffinity),
            sticky: StrategyAny::Sticky(Sticky::new_with_ttl(sticky_ttl, DEFAULT_STICKY_CAP)),
            multi_dim,
        }
    }

    /// Construct with the production defaults for each strategy, including
    /// the compile-time `DEFAULT_STICKY_TTL_SECS` for `Sticky`. Tests that
    /// pin "production default = 30 min" continue to call this.
    pub fn new_default() -> Self {
        Self::new(Duration::from_secs(DEFAULT_STICKY_TTL_SECS))
    }

    /// Alias for [`Self::new_default`], retained for backward compatibility
    /// with existing test call sites.
    pub fn placeholder() -> Self {
        Self::new_default()
    }

    /// Dispatch by [`StrategyKind`] to a borrowed/preconfigured [`StrategyAny`].
    ///
    /// Returns `&StrategyAny` (not `&dyn Strategy`) to stay on enum dispatch
    /// — same shape that the legacy `strategy_for(kind) -> StrategyAny`
    /// factory in `gw-proxy/src/filters/upstream.rs` produces.
    ///
    /// Inline strategies are not stored in the set. Runtime callers must use
    /// [`Self::strategy_for_kind`] so inline `multi_dim` can be constructed
    /// instead of silently falling back to a preconfigured strategy.
    pub fn pick_for(&self, kind: &StrategyKind) -> &StrategyAny {
        match kind {
            StrategyKind::FixedOrder => &self.fixed_order,
            StrategyKind::RoundRobin => &self.round_robin,
            StrategyKind::LoadBalance => &self.load_balance,
            StrategyKind::WeightedRr => &self.weighted_rr,
            StrategyKind::CostFirst => &self.cost_first,
            StrategyKind::LatencyFirst => &self.latency_first,
            StrategyKind::CacheAffinity => &self.cache_affinity,
            StrategyKind::Sticky => &self.sticky,
            StrategyKind::MultiDimInline(_) => {
                panic!("use StrategySet::strategy_for_kind for inline multi_dim")
            }
            StrategyKind::MultiDimPreset(preset) => {
                self.multi_dim.get(preset).unwrap_or_else(|| {
                    // Audit 2026-05-30 §2.1: a model group may reference a multi_dim
                    // preset name that this pod's config does not define (operator
                    // typo, or a preset added to the DB but not yet rolled out to
                    // every pod's config — ordinary control-plane skew). Previously
                    // this `panic!`ed inside upstream_peer, abnormally aborting the
                    // request task. Fail-operational instead: fall back to the always-
                    // present FixedOrder strategy so the request still routes, and
                    // surface the misconfig via a warn-once + metric.
                    if UNKNOWN_PRESETS_WARNED.get(preset).is_none() {
                        UNKNOWN_PRESETS_WARNED.insert(preset.clone(), ());
                        tracing::warn!(
                            code = gw_core::error::codes::GW_ROUTING_UNKNOWN_PRESET,
                            preset = %preset,
                            "unknown multi_dim routing preset referenced by a model group; \
                             falling back to fixed_order. Add it to [routing.presets] on this pod."
                        );
                    }
                    metrics::counter!(
                        "gw_routing_unknown_preset_total",
                        "code" => gw_core::error::codes::GW_ROUTING_UNKNOWN_PRESET,
                    )
                    .increment(1);
                    &self.fixed_order
                })
            }
        }
    }

    pub fn strategy_for_kind(&self, kind: &StrategyKind) -> StrategyChoice<'_> {
        match kind {
            StrategyKind::MultiDimInline(weights) => StrategyChoice::Owned(StrategyAny::MultiDim(
                MultiDim::new("inline".to_string(), dim_weights_from_core(*weights)),
            )),
            _ => StrategyChoice::Borrowed(self.pick_for(kind)),
        }
    }

    pub fn reset_weighted_rr_key(&self, key_id: &KeyId) {
        if let StrategyAny::WeightedRr(weighted_rr) = &self.weighted_rr {
            weighted_rr.reset_key(key_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_presets_warned_caps_at_capacity() {
        for i in 0..2000 {
            UNKNOWN_PRESETS_WARNED.insert(format!("unknown-preset-{i}"), ());
        }
        // run_pending_tasks() so moka's eviction settles.
        UNKNOWN_PRESETS_WARNED.run_pending_tasks();
        assert!(
            UNKNOWN_PRESETS_WARNED.entry_count() <= UNKNOWN_PRESETS_WARNED_CAPACITY,
            "UNKNOWN_PRESETS_WARNED should cap at {}, got {}",
            UNKNOWN_PRESETS_WARNED_CAPACITY,
            UNKNOWN_PRESETS_WARNED.entry_count()
        );
    }
}
