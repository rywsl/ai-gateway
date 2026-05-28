use super::{CandidateFeatures, DimMask, ScoringPolicy, TiebreakStrategy};
use crate::pipeline::policies::{NEUTRAL_LATENCY, PER_KEY_INFLIGHT_CAP};
use crate::strategy::PickCtx;

const DIM_COUNT: f64 = 6.0;
const DEFAULT_LATENCY_CAP_MS: f64 = 60_000.0;
const DEFAULT_COST_CAP: f64 = 1_000.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DimWeights {
    pub health: f64,
    pub latency: f64,
    pub cost: f64,
    pub inflight: f64,
    pub error_rate: f64,
    pub reservation: f64,
}

impl DimWeights {
    pub const fn raw(health: f64, latency: f64, cost: f64, inflight: f64, error_rate: f64) -> Self {
        Self {
            health,
            latency,
            cost,
            inflight,
            error_rate,
            reservation: 0.0,
        }
    }

    pub const fn raw_with_reservation(
        health: f64,
        latency: f64,
        cost: f64,
        inflight: f64,
        error_rate: f64,
        reservation: f64,
    ) -> Self {
        Self {
            health,
            latency,
            cost,
            inflight,
            error_rate,
            reservation,
        }
    }

    pub const fn with_reservation(mut self, reservation: f64) -> Self {
        self.reservation = reservation;
        self
    }

    pub fn normalized(self) -> Self {
        let clean = Self {
            health: clean_weight(self.health),
            latency: clean_weight(self.latency),
            cost: clean_weight(self.cost),
            inflight: clean_weight(self.inflight),
            error_rate: clean_weight(self.error_rate),
            reservation: clean_weight(self.reservation),
        };
        let sum = clean.health
            + clean.latency
            + clean.cost
            + clean.inflight
            + clean.error_rate
            + clean.reservation;
        if sum <= f64::EPSILON {
            return Self::uniform();
        }
        Self {
            health: clean.health / sum,
            latency: clean.latency / sum,
            cost: clean.cost / sum,
            inflight: clean.inflight / sum,
            error_rate: clean.error_rate / sum,
            reservation: clean.reservation / sum,
        }
    }

    pub const fn uniform() -> Self {
        let w = 1.0 / DIM_COUNT;
        Self {
            health: w,
            latency: w,
            cost: w,
            inflight: w,
            error_rate: w,
            reservation: w,
        }
    }
}

fn clean_weight(value: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

#[derive(Clone, Debug)]
pub struct MultiDimPolicy {
    weights: DimWeights,
}

impl MultiDimPolicy {
    pub fn new(weights: DimWeights) -> Self {
        Self {
            weights: weights.normalized(),
        }
    }

    pub fn weights(&self) -> DimWeights {
        self.weights
    }
}

impl ScoringPolicy for MultiDimPolicy {
    fn needed_dims(&self, _ctx: &PickCtx<'_>) -> DimMask {
        let mut dims = DimMask::NONE;
        if self.weights.health > 0.0 {
            dims = dims.union(DimMask::HEALTH);
        }
        if self.weights.latency > 0.0 {
            dims = dims.union(DimMask::LATENCY);
        }
        if self.weights.cost > 0.0 {
            dims = dims.union(DimMask::COST);
        }
        if self.weights.inflight > 0.0 {
            dims = dims.union(DimMask::INFLIGHT);
        }
        if self.weights.error_rate > 0.0 {
            dims = dims.union(DimMask::ERROR_RATE);
        }
        dims
    }

    fn score(&self, features: &CandidateFeatures, _ctx: &PickCtx<'_>) -> f64 {
        self.weights.health * (1.0 - clamp01(features.health_score))
            + self.weights.latency * latency_score(features.latency_p50)
            + self.weights.cost * cost_score(features.cost_weight)
            + self.weights.inflight * inflight_score(features.in_flight)
            + self.weights.error_rate * clamp01(features.error_rate_30s)
            + self.weights.reservation * features.reservation_pressure.map(clamp01).unwrap_or(0.0)
    }

    fn tiebreak(&self) -> TiebreakStrategy {
        TiebreakStrategy::PriorityDesc
    }
}

fn clamp01(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn latency_score(p50: Option<u64>) -> f64 {
    p50.map(|ms| (ms as f64 / DEFAULT_LATENCY_CAP_MS).clamp(0.0, 1.0))
        .unwrap_or(NEUTRAL_LATENCY)
}

fn cost_score(cost_weight: f64) -> f64 {
    if cost_weight.is_finite() {
        (cost_weight / DEFAULT_COST_CAP).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn inflight_score(in_flight: u32) -> f64 {
    (in_flight as f64 / PER_KEY_INFLIGHT_CAP).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::{KeyStatus, VendorKey};
    use gw_core::ids::{KeyId, VendorId};
    use secrecy::SecretString;
    use std::sync::Arc;

    fn fake_key(id: &str, priority: i32) -> Arc<VendorKey> {
        Arc::new(VendorKey {
            key_id: KeyId::from(id),
            vendor_id: VendorId::from("v1"),
            api_key: SecretString::from("sk".to_string()),
            accessible_models: vec![],
            model_mapping: Default::default(),
            accessible_endpoints: vec![],
            protocol_conversion: false,
            priority,
            weight: 1,
            rpm_limit: 0,
            tpm_limit: 0,
            max_usage_count: None,
            proxy: None,
            ip_allowlist: None,
            expires_at: None,
            status: KeyStatus::Active,
            tags: vec![],
            forced_capabilities: vec![],
            exclusive_capability_match: false,
            tunnel_preference: None,
            oauth: None,
            identity_snapshot: None,
        })
    }

    fn features(
        id: &str,
        latency: Option<u64>,
        cost: f64,
        in_flight: u32,
        error_rate: f64,
        reservation_pressure: Option<f64>,
    ) -> CandidateFeatures {
        CandidateFeatures {
            index: 0,
            key: fake_key(id, 0),
            health_score: 1.0,
            latency_p50: latency,
            in_flight,
            cost_weight: cost,
            error_rate_30s: error_rate,
            reservation_pressure,
        }
    }

    #[test]
    fn zero_weights_normalize_to_uniform() {
        assert_eq!(
            MultiDimPolicy::new(DimWeights::raw(0.0, 0.0, 0.0, 0.0, 0.0)).weights(),
            DimWeights::uniform()
        );
    }

    #[test]
    fn single_latency_dimension_prefers_lower_latency() {
        let policy = MultiDimPolicy::new(DimWeights::raw(0.0, 1.0, 0.0, 0.0, 0.0));
        let fast = features("fast", Some(20), 900.0, 100, 0.9, None);
        let slow = features("slow", Some(2_000), 1.0, 0, 0.0, None);
        with_ctx(|ctx| assert!(policy.score(&fast, ctx) < policy.score(&slow, ctx)));
    }

    #[test]
    fn multi_dim_weighting_orders_by_composite_score() {
        let policy = MultiDimPolicy::new(DimWeights::raw(0.0, 0.4, 0.0, 0.4, 0.2));
        let a = features("a", Some(50), 0.0, 100, 0.0, None);
        let b = features("b", Some(5_000), 0.0, 1, 0.5, None);
        with_ctx(|ctx| assert!(policy.score(&a, ctx) < policy.score(&b, ctx)));
    }

    #[test]
    fn reservation_pressure_penalizes_hot_model_when_weighted() {
        let policy = MultiDimPolicy::new(DimWeights::raw_with_reservation(
            0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ));
        let cool = features("cool", Some(50), 0.0, 0, 0.0, Some(0.1));
        let hot = features("hot", Some(50), 0.0, 0, 0.0, Some(0.9));
        with_ctx(|ctx| assert!(policy.score(&cool, ctx) < policy.score(&hot, ctx)));
    }

    fn with_ctx(assertion: impl FnOnce(&PickCtx<'_>)) {
        use crate::latency::LatencyTracker;
        use gw_core::ids::{ModelName, UserId};
        use gw_core::protocol::ProtocolKind;
        let uid = UserId::from("u");
        let model = ModelName::from("m");
        let lt = LatencyTracker::new();
        let ctx = PickCtx {
            user_id: &uid,
            model: &model,
            conversation_id: None,
            message_hash: None,
            api_format: ProtocolKind::OpenAi,
            pricing: None,
            latency_tracker: &lt,
            reservation_view: None,
            frequency_tracker: None,
        };
        assertion(&ctx);
    }
}
