//! Selection strategies (ADR-009). 8 variants implementing the [`Strategy`]
//! trait, plus a dispatching [`StrategyAny`] enum used by the routing pipeline.

use crate::in_flight::InFlightStore;
use crate::latency::LatencyTracker;
use crate::pipeline;
use crate::reservation::{ModelFrequencyTracker, ReservationGlobalView};
use gw_core::entity::Pricing;
use gw_core::entity::VendorKey;
use gw_core::ids::*;
use gw_core::protocol::ApiFormat;
use gw_health::KeyHealthStore;
use std::sync::Arc;

pub mod cache_affinity;
pub mod cost_first;
pub mod fixed_order;
pub mod latency_first;
pub mod load_balance;
pub mod multi_dim;
pub mod round_robin;
pub mod set;
pub mod sticky;
pub mod weighted_rr;

/// Per-request context threaded into every strategy.
pub struct PickCtx<'a> {
    pub user_id: &'a UserId,
    pub model: &'a ModelName,
    pub conversation_id: Option<&'a str>,
    pub message_hash: Option<u64>,
    pub api_format: ApiFormat,
    /// Per-model pricing info for cost-based strategies. `None` when the
    /// Pricing row has not been loaded (fallback to priority-only ranking).
    pub pricing: Option<&'a Pricing>,
    /// Shared latency tracker for latency/error-aware strategies.
    pub latency_tracker: &'a LatencyTracker,
    pub reservation_view: Option<&'a ReservationGlobalView>,
    pub frequency_tracker: Option<&'a ModelFrequencyTracker>,
}

/// Pluggable selection strategy. Implementors must be `Send + Sync` because the
/// routing layer shares one instance across many concurrent requests.
pub trait Strategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        health: &KeyHealthStore,
        in_flight: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>>;
}

/// Enum dispatch over the 8 concrete strategies. Avoids heap-allocated trait
/// objects on the hot path.
pub enum StrategyAny {
    FixedOrder(fixed_order::FixedOrder),
    RoundRobin(round_robin::RoundRobin),
    LoadBalance(load_balance::LoadBalance),
    WeightedRr(weighted_rr::WeightedRr),
    CostFirst(cost_first::CostFirst),
    LatencyFirst(latency_first::LatencyFirst),
    CacheAffinity(cache_affinity::CacheAffinity),
    Sticky(sticky::Sticky),
    MultiDim(multi_dim::MultiDim),
}

impl Strategy for StrategyAny {
    fn name(&self) -> &'static str {
        match self {
            StrategyAny::FixedOrder(s) => s.name(),
            StrategyAny::RoundRobin(s) => s.name(),
            StrategyAny::LoadBalance(s) => s.name(),
            StrategyAny::WeightedRr(s) => s.name(),
            StrategyAny::CostFirst(s) => s.name(),
            StrategyAny::LatencyFirst(s) => s.name(),
            StrategyAny::CacheAffinity(s) => s.name(),
            StrategyAny::Sticky(s) => s.name(),
            StrategyAny::MultiDim(s) => s.name(),
        }
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        if let Some(tracker) = ctx.frequency_tracker {
            tracker.record_pick(ctx.model.as_ref());
        }
        match self {
            StrategyAny::FixedOrder(s) => pipeline::execute(s, c, h, f, ctx),
            StrategyAny::RoundRobin(s) => s.pick(c, h, f, ctx),
            StrategyAny::LoadBalance(s) => s.pick(c, h, f, ctx),
            StrategyAny::WeightedRr(s) => s.pick(c, h, f, ctx),
            StrategyAny::CostFirst(s) => pipeline::execute(s, c, h, f, ctx),
            StrategyAny::LatencyFirst(s) => pipeline::execute(s, c, h, f, ctx),
            StrategyAny::CacheAffinity(s) => s.pick(c, h, f, ctx),
            StrategyAny::Sticky(s) => s.pick(c, h, f, ctx),
            StrategyAny::MultiDim(s) => pipeline::execute(s, c, h, f, ctx),
        }
    }
}
