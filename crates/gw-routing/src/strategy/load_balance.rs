//! `LoadBalance` — pick the available key with the lowest composite
//! score `α·in_flight_ratio + β·p50_ttft_norm + γ·error_rate`.

use super::*;
use crate::pipeline::policies::{
    LoadBalancePolicy, LOAD_BALANCE_ALPHA, LOAD_BALANCE_BETA, LOAD_BALANCE_GAMMA,
    PER_KEY_INFLIGHT_CAP,
};
use crate::pipeline::{CandidateFeatures, DimMask, ScoringPolicy};

pub struct LoadBalance;

impl Strategy for LoadBalance {
    fn name(&self) -> &'static str {
        "load_balance"
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        let features = crate::pipeline::extract(c, h, f, ctx, self.needed_dims(ctx));
        if features.is_empty() {
            return None;
        }

        let max_p50 = features
            .iter()
            .filter_map(|feature| feature.latency_p50)
            .max()
            .unwrap_or(1) as f64;
        let policy = LoadBalancePolicy::new(max_p50);
        crate::pipeline::pick_best_index(&policy, &features, ctx).map(|i| &c[i])
    }
}

impl ScoringPolicy for LoadBalance {
    fn needed_dims(&self, _ctx: &PickCtx<'_>) -> DimMask {
        DimMask::HEALTH
            .union(DimMask::INFLIGHT)
            .union(DimMask::LATENCY)
            .union(DimMask::ERROR_RATE)
    }

    fn score(&self, features: &CandidateFeatures, _ctx: &PickCtx<'_>) -> f64 {
        LOAD_BALANCE_ALPHA * (features.in_flight as f64 / PER_KEY_INFLIGHT_CAP)
            + LOAD_BALANCE_BETA * features.latency_p50.map(|ms| ms as f64).unwrap_or(0.0)
            + LOAD_BALANCE_GAMMA * features.error_rate_30s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::latency::LatencyTracker;
    use gw_core::entity::*;
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::protocol::ProtocolKind;
    use secrecy::SecretString;
    use std::time::Duration;

    fn fake_key(id: &str) -> Arc<VendorKey> {
        Arc::new(VendorKey {
            key_id: KeyId::from(id),
            vendor_id: VendorId::from("v1"),
            api_key: SecretString::from("sk".to_string()),
            accessible_models: vec![],
            model_mapping: Default::default(),
            accessible_endpoints: vec![],
            protocol_conversion: false,
            priority: 0,
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
            quota_state: Default::default(),
        })
    }

    /// Score key_a: 0.4*(100/1024) + 0.35*(10/200) + 0.25*0.0 ≈ 0.057
    /// Score key_b: 0.4*(2/1024) + 0.35*(200/200) + 0.25*0.3 ≈ 0.426
    /// key_a wins (lower score).
    #[test]
    fn test_three_dim_scoring() {
        let keys = vec![fake_key("a"), fake_key("b")];
        let h = KeyHealthStore::default();
        let f = InFlightStore::default();
        let lt = LatencyTracker::new();
        let uid = UserId::from("u");
        let model = ModelName::from("m");
        let fmt = ProtocolKind::OpenAi;

        // key_a: 100 in-flight, 10ms latency, 0 errors
        f.get_or_create(&keys[0].key_id)
            .count
            .store(100, std::sync::atomic::Ordering::Relaxed);
        for _ in 0..10 {
            lt.record_ttft(&keys[0].key_id, fmt, Duration::from_millis(10));
        }

        // key_b: 2 in-flight, 200ms latency, 30% errors (7 ok + 3 err)
        f.get_or_create(&keys[1].key_id)
            .count
            .store(2, std::sync::atomic::Ordering::Relaxed);
        for _ in 0..7 {
            lt.record_ttft(&keys[1].key_id, fmt, Duration::from_millis(200));
        }
        for _ in 0..3 {
            lt.record_error(&keys[1].key_id, fmt);
        }

        let ctx = PickCtx {
            user_id: &uid,
            model: &model,
            conversation_id: None,
            message_hash: None,
            api_format: fmt,
            pricing: None,
            latency_tracker: &lt,
            reservation_view: None,
            frequency_tracker: None,
        };
        let picked = LoadBalance.pick(&keys, &h, &f, &ctx).unwrap();
        assert_eq!(
            picked.key_id.as_ref(),
            "a",
            "key_a has lower composite score despite higher in-flight"
        );
    }

    /// When no latency data exists, scores differentiate only by in-flight
    /// ratio (β dimension uses NEUTRAL=0.5 for all, γ=0.0 for all).
    #[test]
    fn test_all_neutral() {
        let keys = vec![fake_key("a"), fake_key("b")];
        let h = KeyHealthStore::default();
        let f = InFlightStore::default();
        let lt = LatencyTracker::new();
        let uid = UserId::from("u");
        let model = ModelName::from("m");

        f.get_or_create(&keys[0].key_id)
            .count
            .store(50, std::sync::atomic::Ordering::Relaxed);
        f.get_or_create(&keys[1].key_id)
            .count
            .store(10, std::sync::atomic::Ordering::Relaxed);

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
        let picked = LoadBalance.pick(&keys, &h, &f, &ctx).unwrap();
        assert_eq!(
            picked.key_id.as_ref(),
            "b",
            "key_b wins due to lower in-flight when latency is neutral"
        );
    }

    #[test]
    fn test_empty_candidates() {
        let keys: Vec<Arc<VendorKey>> = vec![];
        let h = KeyHealthStore::default();
        let f = InFlightStore::default();
        let lt = LatencyTracker::new();
        let uid = UserId::from("u");
        let model = ModelName::from("m");
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
        assert!(LoadBalance.pick(&keys, &h, &f, &ctx).is_none());
    }
}
