use super::{CandidateFeatures, DimMask, ScoringPolicy, TiebreakStrategy};
use crate::strategy::PickCtx;

pub const PER_KEY_INFLIGHT_CAP: f64 = 1024.0;
pub const LOAD_BALANCE_ALPHA: f64 = 0.40;
pub const LOAD_BALANCE_BETA: f64 = 0.35;
pub const LOAD_BALANCE_GAMMA: f64 = 0.25;
pub const NEUTRAL_LATENCY: f64 = 0.5;

pub struct FixedOrderPolicy;

impl ScoringPolicy for FixedOrderPolicy {
    fn needed_dims(&self, _ctx: &PickCtx<'_>) -> DimMask {
        DimMask::HEALTH_ONLY
    }

    fn score(&self, features: &CandidateFeatures, _ctx: &PickCtx<'_>) -> f64 {
        -(features.key.priority as f64)
    }
}

pub struct CostFirstPolicy;

impl ScoringPolicy for CostFirstPolicy {
    fn needed_dims(&self, ctx: &PickCtx<'_>) -> DimMask {
        if ctx.pricing.is_some() {
            DimMask::HEALTH.union(DimMask::COST)
        } else {
            DimMask::HEALTH_ONLY
        }
    }

    fn score(&self, features: &CandidateFeatures, ctx: &PickCtx<'_>) -> f64 {
        if ctx.pricing.is_some() {
            features.cost_weight
        } else {
            -(features.key.priority as f64)
        }
    }
}

pub struct LatencyFirstPolicy;

impl ScoringPolicy for LatencyFirstPolicy {
    fn needed_dims(&self, _ctx: &PickCtx<'_>) -> DimMask {
        DimMask::HEALTH.union(DimMask::LATENCY)
    }

    fn score(&self, features: &CandidateFeatures, _ctx: &PickCtx<'_>) -> f64 {
        features.latency_p50.map(|ms| ms as f64).unwrap_or(f64::MAX)
    }

    fn tiebreak(&self) -> TiebreakStrategy {
        TiebreakStrategy::PriorityDesc
    }
}

pub struct LoadBalancePolicy {
    max_p50: f64,
}

impl LoadBalancePolicy {
    pub fn new(max_p50: f64) -> Self {
        Self {
            max_p50: max_p50.max(1.0),
        }
    }
}

impl ScoringPolicy for LoadBalancePolicy {
    fn needed_dims(&self, _ctx: &PickCtx<'_>) -> DimMask {
        DimMask::HEALTH
            .union(DimMask::INFLIGHT)
            .union(DimMask::LATENCY)
            .union(DimMask::ERROR_RATE)
    }

    fn score(&self, features: &CandidateFeatures, _ctx: &PickCtx<'_>) -> f64 {
        let in_flight_ratio = features.in_flight as f64 / PER_KEY_INFLIGHT_CAP;
        let p50_norm = features
            .latency_p50
            .map(|ms| ms as f64 / self.max_p50)
            .unwrap_or(NEUTRAL_LATENCY);
        LOAD_BALANCE_ALPHA * in_flight_ratio
            + LOAD_BALANCE_BETA * p50_norm
            + LOAD_BALANCE_GAMMA * features.error_rate_30s
    }
}
