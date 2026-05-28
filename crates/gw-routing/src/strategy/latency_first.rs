//! `LatencyFirst` strategy: pick the available key with the lowest P50 TTFT.

use super::*;
use crate::pipeline::policies::LatencyFirstPolicy;
use crate::pipeline::{CandidateFeatures, DimMask, ScoringPolicy, TiebreakStrategy};

pub struct LatencyFirst;

impl Strategy for LatencyFirst {
    fn name(&self) -> &'static str {
        "latency_first"
    }

    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        crate::pipeline::execute(self, candidates, h, f, ctx)
    }
}

impl ScoringPolicy for LatencyFirst {
    fn needed_dims(&self, ctx: &PickCtx<'_>) -> DimMask {
        LatencyFirstPolicy.needed_dims(ctx)
    }

    fn score(&self, features: &CandidateFeatures, ctx: &PickCtx<'_>) -> f64 {
        LatencyFirstPolicy.score(features, ctx)
    }

    fn tiebreak(&self) -> TiebreakStrategy {
        TiebreakStrategy::PriorityDesc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::KeyStatus;
    use gw_core::protocol::ProtocolKind;
    use gw_health::cooldown::{CooldownDirective, CooldownReason};
    use secrecy::SecretString;
    use std::time::Duration;

    fn fake_key(id: &str, prio: i32) -> Arc<VendorKey> {
        Arc::new(VendorKey {
            key_id: KeyId::from(id),
            vendor_id: VendorId::from("v1"),
            api_key: SecretString::from("sk".to_string()),
            accessible_models: vec![],
            model_mapping: Default::default(),
            accessible_endpoints: vec![],
            protocol_conversion: false,
            priority: prio,
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

    fn seed_p50(lt: &LatencyTracker, key_id: &str, ms: u64) {
        let kid = KeyId::from(key_id);
        for _ in 0..10 {
            lt.record_ttft(&kid, ProtocolKind::OpenAi, Duration::from_millis(ms));
        }
    }

    #[test]
    fn test_picks_lowest_p50() {
        let lt = LatencyTracker::new();
        seed_p50(&lt, "a", 200);
        seed_p50(&lt, "b", 50);
        seed_p50(&lt, "c", 150);

        let keys = vec![fake_key("a", 10), fake_key("b", 10), fake_key("c", 10)];
        let h = KeyHealthStore::default();
        let f = InFlightStore::default();
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

        let picked = LatencyFirst.pick(&keys, &h, &f, &ctx).unwrap();
        assert_eq!(picked.key_id.as_ref(), "b");
    }

    #[test]
    fn test_cold_start_fallback() {
        let lt = LatencyTracker::new();
        let keys = vec![fake_key("a", 5), fake_key("b", 20), fake_key("c", 10)];
        let h = KeyHealthStore::default();
        let f = InFlightStore::default();
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

        let picked = LatencyFirst.pick(&keys, &h, &f, &ctx).unwrap();
        assert_eq!(picked.key_id.as_ref(), "b");
    }

    #[test]
    fn test_skips_unavailable() {
        let lt = LatencyTracker::new();
        seed_p50(&lt, "a", 10);
        seed_p50(&lt, "b", 100);
        seed_p50(&lt, "c", 200);

        let keys = vec![fake_key("a", 10), fake_key("b", 10), fake_key("c", 10)];
        let h = KeyHealthStore::default();
        let f = InFlightStore::default();

        h.apply_cooldown(
            &KeyId::from("a"),
            ProtocolKind::OpenAi,
            CooldownDirective {
                reason: CooldownReason::Status429RateLimit,
                ttl_secs: 3600,
                reset_consecutive: false,
            },
        );

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
        let picked = LatencyFirst.pick(&keys, &h, &f, &ctx).unwrap();
        assert_eq!(picked.key_id.as_ref(), "b");
    }

    #[test]
    fn test_tie_break_by_priority() {
        let lt = LatencyTracker::new();
        seed_p50(&lt, "a", 100);
        seed_p50(&lt, "b", 100);

        let keys = vec![fake_key("a", 5), fake_key("b", 20)];
        let h = KeyHealthStore::default();
        let f = InFlightStore::default();
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

        let picked = LatencyFirst.pick(&keys, &h, &f, &ctx).unwrap();
        assert_eq!(picked.key_id.as_ref(), "b");
    }
}
