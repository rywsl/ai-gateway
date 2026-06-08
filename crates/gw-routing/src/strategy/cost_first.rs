//! `CostFirst` strategy: pick the cheapest available key by
//! `priority × model_markup` score (ascending — lower = cheaper).
//!
//! When `ctx.pricing` is `None` (pricing row not loaded), falls back to
//! priority-descending ordering (identical to the v1 behaviour).

use super::*;
use crate::pipeline::policies::CostFirstPolicy;
use crate::pipeline::{CandidateFeatures, DimMask, ScoringPolicy};

pub struct CostFirst;

impl Strategy for CostFirst {
    fn name(&self) -> &'static str {
        "cost_first"
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

impl ScoringPolicy for CostFirst {
    fn needed_dims(&self, ctx: &PickCtx<'_>) -> DimMask {
        CostFirstPolicy.needed_dims(ctx)
    }

    fn score(&self, features: &CandidateFeatures, ctx: &PickCtx<'_>) -> f64 {
        CostFirstPolicy.score(features, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::{KeyStatus, Pricing};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::protocol::ProtocolKind;
    use gw_health::cooldown::{CooldownDirective, CooldownReason};
    use secrecy::SecretString;

    fn make_key(key_id: &str, priority: i32) -> Arc<VendorKey> {
        Arc::new(VendorKey {
            key_id: KeyId::from(key_id),
            vendor_id: VendorId::from("vendor-test"),
            api_key: SecretString::from("sk-test".to_string()),
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
            quota_state: Default::default(),
        })
    }

    #[test]
    fn test_picks_lowest_cost_with_pricing() {
        let strategy = CostFirst;
        let candidates = vec![
            make_key("key-a", 1),
            make_key("key-b", 5),
            make_key("key-c", 3),
        ];
        let health = KeyHealthStore::default();
        let inflight = InFlightStore::default();
        let lt = LatencyTracker::default();
        let uid = UserId::from("user-1");
        let model = ModelName::from("gpt-4");
        let pricing = Pricing {
            model: ModelName::from("gpt-4"),
            input_price_micro_per_token: 0,
            output_price_micro_per_token: 0,
            max_output: 0,
            model_markup: 1.2,
            connect_timeout_ms: None,
            read_timeout_ms: None,
            total_timeout_ms: None,
            stream_total_timeout_ms: None,
        };
        let ctx = PickCtx {
            user_id: &uid,
            model: &model,
            conversation_id: None,
            message_hash: None,
            api_format: ProtocolKind::OpenAi,
            pricing: Some(&pricing),
            latency_tracker: &lt,
            reservation_view: None,
            frequency_tracker: None,
        };

        let picked = strategy.pick(&candidates, &health, &inflight, &ctx);
        assert_eq!(picked.unwrap().key_id.as_ref(), "key-a");
    }

    #[test]
    fn test_fallback_without_pricing() {
        let strategy = CostFirst;
        let candidates = vec![
            make_key("key-a", 1),
            make_key("key-b", 5),
            make_key("key-c", 3),
        ];
        let health = KeyHealthStore::default();
        let inflight = InFlightStore::default();
        let lt = LatencyTracker::default();
        let uid = UserId::from("user-1");
        let model = ModelName::from("gpt-4");
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

        let picked = strategy.pick(&candidates, &health, &inflight, &ctx);
        assert_eq!(picked.unwrap().key_id.as_ref(), "key-b");
    }

    #[test]
    fn test_skips_unavailable() {
        let strategy = CostFirst;
        let candidates = vec![
            make_key("key-a", 1),
            make_key("key-b", 5),
            make_key("key-c", 3),
        ];
        let health = KeyHealthStore::default();
        health.apply_cooldown(
            &candidates[0].key_id,
            ProtocolKind::OpenAi,
            CooldownDirective {
                reason: CooldownReason::Status429RateLimit,
                ttl_secs: 3600,
                reset_consecutive: false,
            },
        );
        let inflight = InFlightStore::default();
        let lt = LatencyTracker::default();
        let uid = UserId::from("user-1");
        let model = ModelName::from("gpt-4");
        let pricing = Pricing {
            model: ModelName::from("gpt-4"),
            input_price_micro_per_token: 0,
            output_price_micro_per_token: 0,
            max_output: 0,
            model_markup: 1.0,
            connect_timeout_ms: None,
            read_timeout_ms: None,
            total_timeout_ms: None,
            stream_total_timeout_ms: None,
        };
        let ctx = PickCtx {
            user_id: &uid,
            model: &model,
            conversation_id: None,
            message_hash: None,
            api_format: ProtocolKind::OpenAi,
            pricing: Some(&pricing),
            latency_tracker: &lt,
            reservation_view: None,
            frequency_tracker: None,
        };

        let picked = strategy.pick(&candidates, &health, &inflight, &ctx);
        assert_eq!(picked.unwrap().key_id.as_ref(), "key-c");
    }
}
