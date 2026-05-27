use crate::in_flight::InFlightStore;
use crate::latency::{LatencySnapshot, LatencyTracker};
use crate::strategy::PickCtx;
use gw_core::entity::VendorKey;
use gw_health::KeyHealthStore;
use std::sync::Arc;

pub mod multi_dim;
pub mod policies;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DimMask(u8);

impl DimMask {
    pub const NONE: Self = Self(0);
    pub const HEALTH: Self = Self(1 << 0);
    pub const LATENCY: Self = Self(1 << 1);
    pub const INFLIGHT: Self = Self(1 << 2);
    pub const COST: Self = Self(1 << 3);
    pub const ERROR_RATE: Self = Self(1 << 4);

    pub const HEALTH_ONLY: Self = Self::HEALTH;

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TiebreakStrategy {
    InputOrder,
    PriorityDesc,
    PriorityAsc,
}

#[derive(Clone, Debug)]
pub struct CandidateFeatures {
    pub index: usize,
    pub key: Arc<VendorKey>,
    pub health_score: f64,
    pub latency_p50: Option<u64>,
    pub in_flight: u32,
    pub cost_weight: f64,
    pub error_rate_30s: f64,
    pub reservation_pressure: Option<f64>,
}

impl CandidateFeatures {
    pub fn key_id(&self) -> &gw_core::ids::KeyId {
        &self.key.key_id
    }
}

pub trait ScoringPolicy: Send + Sync {
    fn needed_dims(&self, ctx: &PickCtx<'_>) -> DimMask;

    fn score(&self, features: &CandidateFeatures, ctx: &PickCtx<'_>) -> f64;

    fn tiebreak(&self) -> TiebreakStrategy {
        TiebreakStrategy::InputOrder
    }

    fn gate(&self, _features: &CandidateFeatures, _ctx: &PickCtx<'_>) -> bool {
        true
    }

    fn before_extract(&self, _candidates: &[Arc<VendorKey>], _ctx: &PickCtx<'_>) {}

    fn after_pick(&self, _picked: &CandidateFeatures, _ctx: &PickCtx<'_>) {}
}

pub fn execute<'a, P: ScoringPolicy>(
    policy: &P,
    candidates: &'a [Arc<VendorKey>],
    health: &KeyHealthStore,
    in_flight: &InFlightStore,
    ctx: &PickCtx<'_>,
) -> Option<&'a Arc<VendorKey>> {
    if candidates.is_empty() {
        return None;
    }

    policy.before_extract(candidates, ctx);
    let dims = policy.needed_dims(ctx).union(DimMask::HEALTH);
    let mut features = extract_features(candidates, health, in_flight, ctx, dims);
    if features.is_empty() {
        return None;
    }

    let mut best: Option<(usize, f64)> = None;
    for (feature_idx, feature) in features.iter().enumerate() {
        if let Some(view) = ctx.reservation_view {
            if !view.gate_allows(ctx.model) {
                continue;
            }
        }
        if !policy.gate(feature, ctx) {
            continue;
        }
        let score = policy.score(feature, ctx);
        match best {
            None => best = Some((feature_idx, score)),
            Some((best_idx, best_score)) => {
                if is_better(
                    policy.tiebreak(),
                    feature,
                    score,
                    &features[best_idx],
                    best_score,
                ) {
                    best = Some((feature_idx, score));
                }
            }
        }
    }

    let (best_idx, _) = best?;
    let picked = features.swap_remove(best_idx);
    policy.after_pick(&picked, ctx);
    Some(&candidates[picked.index])
}

pub fn extract(
    candidates: &[Arc<VendorKey>],
    health: &KeyHealthStore,
    in_flight: &InFlightStore,
    ctx: &PickCtx<'_>,
    dims: DimMask,
) -> Vec<CandidateFeatures> {
    extract_features(
        candidates,
        health,
        in_flight,
        ctx,
        dims.union(DimMask::HEALTH),
    )
}

pub fn pick_best_index<P: ScoringPolicy>(
    policy: &P,
    features: &[CandidateFeatures],
    ctx: &PickCtx<'_>,
) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (feature_idx, feature) in features.iter().enumerate() {
        if let Some(view) = ctx.reservation_view {
            if !view.gate_allows(ctx.model) {
                continue;
            }
        }
        if !policy.gate(feature, ctx) {
            continue;
        }
        let score = policy.score(feature, ctx);
        match best {
            None => best = Some((feature_idx, score)),
            Some((best_idx, best_score)) => {
                if is_better(
                    policy.tiebreak(),
                    feature,
                    score,
                    &features[best_idx],
                    best_score,
                ) {
                    best = Some((feature_idx, score));
                }
            }
        }
    }
    best.map(|(feature_idx, _)| features[feature_idx].index)
}

fn extract_features(
    candidates: &[Arc<VendorKey>],
    health: &KeyHealthStore,
    in_flight: &InFlightStore,
    ctx: &PickCtx<'_>,
    dims: DimMask,
) -> Vec<CandidateFeatures> {
    let mut latency = Vec::<LatencySnapshot>::new();
    if dims.contains(DimMask::LATENCY) || dims.contains(DimMask::ERROR_RATE) {
        latency = ctx
            .latency_tracker
            .snapshot_for_pairs(candidates.iter().map(|key| (&key.key_id, ctx.api_format)));
    }

    let mut in_flight_counts = Vec::new();
    if dims.contains(DimMask::INFLIGHT) {
        in_flight_counts = in_flight.snapshot_counts(candidates.iter().map(|key| &key.key_id));
    }

    let markup = ctx.pricing.map(|p| p.model_markup as f64).unwrap_or(1.0);
    let mut out = Vec::with_capacity(candidates.len());
    for (index, key) in candidates.iter().enumerate() {
        if dims.contains(DimMask::HEALTH) && !health.is_available(&key.key_id, ctx.api_format) {
            continue;
        }
        let latency_entry = latency.get(index);
        let p50 = latency_entry.and_then(|entry| entry.p50_ttft_ms);
        let error_rate = latency_entry
            .map(|entry| entry.error_rate_30s)
            .unwrap_or(0.0);
        let inflight = in_flight_counts
            .get(index)
            .map(|entry| entry.count)
            .unwrap_or(0);
        out.push(CandidateFeatures {
            index,
            key: Arc::clone(key),
            health_score: 1.0,
            latency_p50: p50,
            in_flight: inflight,
            cost_weight: key.priority as f64 * markup,
            error_rate_30s: error_rate,
            reservation_pressure: ctx
                .reservation_view
                .and_then(|view| view.pressure_for(ctx.model)),
        });
    }
    out
}

fn is_better(
    tiebreak: TiebreakStrategy,
    candidate: &CandidateFeatures,
    candidate_score: f64,
    best: &CandidateFeatures,
    best_score: f64,
) -> bool {
    match candidate_score.partial_cmp(&best_score) {
        Some(std::cmp::Ordering::Less) => true,
        Some(std::cmp::Ordering::Greater) => false,
        _ => match tiebreak {
            TiebreakStrategy::InputOrder => candidate.index < best.index,
            TiebreakStrategy::PriorityDesc => candidate
                .key
                .priority
                .cmp(&best.key.priority)
                .then_with(|| best.index.cmp(&candidate.index))
                .is_gt(),
            TiebreakStrategy::PriorityAsc => candidate
                .key
                .priority
                .cmp(&best.key.priority)
                .then_with(|| candidate.index.cmp(&best.index).reverse())
                .is_lt(),
        },
    }
}

#[allow(dead_code)]
fn _assert_latency_tracker_send_sync(_: &LatencyTracker) {}
