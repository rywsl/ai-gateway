//! Selection strategies (ADR-009). 8 variants implementing the [`Strategy`] trait,
//! plus a dispatching [`StrategyAny`] enum used by the routing pipeline.

use crate::breaker::BreakerStore;
use gw_core::entity::VendorKey;
use gw_core::ids::*;
use std::sync::Arc;

pub mod cache_affinity;
pub mod cost_first;
pub mod fixed_order;
pub mod latency_first;
pub mod load_balance;
pub mod round_robin;
pub mod sticky;
pub mod weighted_rr;

/// Per-request context threaded into every strategy.
pub struct PickCtx<'a> {
    pub user_id: &'a UserId,
    pub model: &'a ModelName,
    pub conversation_id: Option<&'a str>,
    pub message_hash: Option<u64>,
}

/// Pluggable selection strategy. Implementors must be `Send + Sync` because the
/// routing layer shares one instance across many concurrent requests.
pub trait Strategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        breakers: &BreakerStore,
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
        }
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        b: &BreakerStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        match self {
            StrategyAny::FixedOrder(s) => s.pick(c, b, ctx),
            StrategyAny::RoundRobin(s) => s.pick(c, b, ctx),
            StrategyAny::LoadBalance(s) => s.pick(c, b, ctx),
            StrategyAny::WeightedRr(s) => s.pick(c, b, ctx),
            StrategyAny::CostFirst(s) => s.pick(c, b, ctx),
            StrategyAny::LatencyFirst(s) => s.pick(c, b, ctx),
            StrategyAny::CacheAffinity(s) => s.pick(c, b, ctx),
            StrategyAny::Sticky(s) => s.pick(c, b, ctx),
        }
    }
}
