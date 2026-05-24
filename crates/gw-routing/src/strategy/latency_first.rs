//! `LatencyFirst` strategy: pick the available key with the lowest P50 TTFT.

use super::*;

pub struct LatencyFirst;

impl Strategy for LatencyFirst {
    fn name(&self) -> &'static str {
        "latency_first"
    }

    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        _f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        // Build (index, Option<p50_ms>) for each available candidate.
        let mut scored: Vec<(usize, Option<u64>)> = candidates
            .iter()
            .enumerate()
            .filter(|(_, k)| h.is_available(&k.key_id, ctx.api_format))
            .map(|(i, k)| {
                let p50 = ctx.latency_tracker.p50_ttft_ms(&k.key_id, ctx.api_format);
                (i, p50)
            })
            .collect();

        // Sort: keys with P50 data first (ascending), then keys without data (cold start).
        // Tie-break: higher priority wins (descending).
        scored.sort_by(|a, b| match (a.1, b.1) {
            (Some(pa), Some(pb)) => pa
                .cmp(&pb)
                .then_with(|| candidates[b.0].priority.cmp(&candidates[a.0].priority)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => candidates[b.0].priority.cmp(&candidates[a.0].priority),
        });

        scored.first().map(|(i, _)| &candidates[*i])
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
        };

        let picked = LatencyFirst.pick(&keys, &h, &f, &ctx).unwrap();
        assert_eq!(picked.key_id.as_ref(), "b");
    }
}
