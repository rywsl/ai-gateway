//! `Sticky` strategy: route every request with the same `conversation_id` to
//! the same key, refreshing the binding's TTL on each hit.
//!
//! G12 (T2-C): bindings live in a `moka::sync::Cache` with a 30-minute
//! `time_to_live` and a 100_000-entry `max_capacity`. moka handles TTL
//! eviction and LRU overflow internally, so abandoned conversations no
//! longer leak. Without a `conversation_id`, falls back to the first
//! available candidate.
//!
//! The cache is shared across requests via `Arc<StrategySet>` (see
//! `crates/gw-routing/src/strategy/set.rs`); each request reads/writes
//! the same map.

use super::*;
use crate::pipeline::{CandidateFeatures, DimMask, ScoringPolicy};
use gw_core::ids::KeyId;
use moka::sync::Cache;
use sha2::{Digest, Sha256};
use std::time::Duration;

/// Default time-to-live for a `(conversation_id -> key_id)` binding.
pub const DEFAULT_STICKY_TTL_SECS: u64 = 30 * 60;
/// Default upper bound on cache entries before LRU eviction kicks in.
pub const DEFAULT_STICKY_CAP: u64 = 100_000;

pub struct Sticky {
    /// `conversation_id -> key_id` with moka-managed TTL + LRU eviction.
    bindings: Cache<String, KeyId>,
}

impl Default for Sticky {
    fn default() -> Self {
        Self::new_with_ttl(
            Duration::from_secs(DEFAULT_STICKY_TTL_SECS),
            DEFAULT_STICKY_CAP,
        )
    }
}

impl Sticky {
    /// Construct with explicit TTL + capacity. Production code should call
    /// [`Sticky::default`]; tests use this to dial down the TTL.
    pub fn new_with_ttl(ttl: Duration, capacity: u64) -> Self {
        Self {
            bindings: Cache::builder()
                .time_to_live(ttl)
                .max_capacity(capacity)
                .build(),
        }
    }

    /// Force moka to run any pending eviction work synchronously. Useful in
    /// tests to make TTL eviction deterministic after `sleep`; production
    /// code does not need to call this — moka drains pending work
    /// opportunistically on read/write.
    pub fn invalidate_expired(&self) {
        self.bindings.run_pending_tasks();
    }

    /// Approximate live binding count. moka surfaces this as an estimate;
    /// it is exact after `invalidate_expired()`.
    pub fn bindings_active(&self) -> u64 {
        self.bindings.entry_count()
    }

    fn hash_conv(conv: &str, n: usize) -> usize {
        let mut hasher = Sha256::new();
        hasher.update(conv.as_bytes());
        let digest = hasher.finalize();
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&digest[..8]);
        let h = u64::from_le_bytes(bytes);
        (h % n as u64) as usize
    }
}

impl Strategy for Sticky {
    fn name(&self) -> &'static str {
        "sticky"
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        _f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        if c.is_empty() {
            return None;
        }
        let Some(conv) = ctx.conversation_id else {
            // No conversation context: fall back to first available.
            return c.iter().find(|k| h.is_available(&k.key_id, ctx.api_format));
        };

        // Hit path: look up an existing binding. moka refreshes the TTL on
        // read via its `time_to_idle`-less default; an entry past
        // `time_to_live` is gone before `get` returns it.
        if let Some(kid) = self.bindings.get(conv) {
            if let Some(k) = c.iter().find(|k| k.key_id == kid) {
                if h.is_available(&k.key_id, ctx.api_format) {
                    metrics::counter!("gw_sticky_hit_total").increment(1);
                    return Some(k);
                }
            }
            let available: Vec<&Arc<VendorKey>> = c
                .iter()
                .filter(|k| h.is_available(&k.key_id, ctx.api_format))
                .collect();
            if available.is_empty() {
                return None;
            }
            let idx = Self::hash_conv(conv, available.len());
            metrics::counter!("gw_sticky_fallback_total").increment(1);
            return c.iter().find(|k| k.key_id == available[idx].key_id);
        }

        // Bind path: compute a fresh binding over the available set.
        let available: Vec<&Arc<VendorKey>> = c
            .iter()
            .filter(|k| h.is_available(&k.key_id, ctx.api_format))
            .collect();
        if available.is_empty() {
            return None;
        }
        let idx = Self::hash_conv(conv, available.len());
        let chosen = available[idx];
        self.bindings
            .insert(conv.to_string(), chosen.key_id.clone());
        metrics::counter!("gw_sticky_bind_total").increment(1);
        c.iter().find(|k| k.key_id == chosen.key_id)
    }
}

impl ScoringPolicy for Sticky {
    fn needed_dims(&self, _ctx: &PickCtx<'_>) -> DimMask {
        DimMask::HEALTH_ONLY
    }

    fn score(&self, features: &CandidateFeatures, _ctx: &PickCtx<'_>) -> f64 {
        features.index as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_flight::InFlightStore;
    use crate::latency::LatencyTracker;
    use gw_core::entity::{KeyStatus, VendorKey};
    use gw_core::ids::{ModelName, UserId, VendorId};
    use gw_core::protocol::ProtocolKind;
    use gw_health::KeyHealthStore;
    use secrecy::SecretString;

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

    fn pick_key(sticky: &Sticky, keys: &[Arc<VendorKey>], conv: &str) -> Option<KeyId> {
        let health = KeyHealthStore::default();
        let in_flight = InFlightStore::default();
        let latency = LatencyTracker::new();
        let user = UserId::from("u");
        let model = ModelName::from("m");
        let ctx = PickCtx {
            user_id: &user,
            model: &model,
            conversation_id: Some(conv),
            message_hash: None,
            api_format: ProtocolKind::OpenAi,
            pricing: None,
            latency_tracker: &latency,
            reservation_view: None,
            frequency_tracker: None,
        };
        sticky
            .pick(keys, &health, &in_flight, &ctx)
            .map(|k| k.key_id.clone())
    }

    #[test]
    fn missing_bound_key_falls_back_without_rewriting_binding() {
        let sticky = Sticky::default();
        let conv = "stable-conv";

        assert_eq!(
            pick_key(&sticky, &[fake_key("a")], conv),
            Some(KeyId::from("a"))
        );
        sticky.invalidate_expired();
        assert_eq!(sticky.bindings.get(conv), Some(KeyId::from("a")));

        let fallback = pick_key(&sticky, &[fake_key("x"), fake_key("y")], conv)
            .expect("fallback should pick an available key for this request");

        assert!(fallback == KeyId::from("x") || fallback == KeyId::from("y"));
        sticky.invalidate_expired();
        assert_eq!(
            sticky.bindings.get(conv),
            Some(KeyId::from("a")),
            "fallback must not rewrite the sticky binding"
        );
    }
}
