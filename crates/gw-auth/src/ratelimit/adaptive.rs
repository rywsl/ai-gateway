use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use gw_core::ids::KeyId;
use gw_core::ratelimit::RateLimitKind;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn set_adaptive_state_metrics(state: &KeyAdaptiveState) {
    metrics::gauge!("gw_adaptive_rpm_confidence").set(state.confidence.value as f64);
    metrics::gauge!("gw_adaptive_rpm_learned_value").set(state.learned_rpm.unwrap_or(0) as f64);
}

const DEFAULT_ADAPTIVE_CAPACITY: u64 = 10_000;
const DEFAULT_ADAPTIVE_TTI: std::time::Duration = std::time::Duration::from_secs(3600);

pub struct AdaptiveRpmManager {
    pub state: moka::sync::Cache<KeyId, Arc<KeyAdaptiveState>>,
    /// Per-vendor-key GCRA limiter enforcing the learned ceiling. Cached and
    /// rebuilt only when the enforced limit changes (audit 2026-05-30 §1.8).
    /// The `u32` is the per-minute limit the cached limiter was built for.
    limiters: moka::sync::Cache<KeyId, (u32, Arc<DefaultDirectRateLimiter>)>,
    pub cfg: AdaptiveCfg,
}

impl AdaptiveRpmManager {
    pub fn new(cfg: AdaptiveCfg) -> Self {
        Self::with_capacity_and_tti(cfg, DEFAULT_ADAPTIVE_CAPACITY, DEFAULT_ADAPTIVE_TTI)
    }

    pub fn with_capacity_and_tti(
        cfg: AdaptiveCfg,
        capacity: u64,
        tti: std::time::Duration,
    ) -> Self {
        Self {
            state: moka::sync::Cache::builder()
                .max_capacity(capacity)
                .time_to_idle(tti)
                .build(),
            limiters: moka::sync::Cache::builder()
                .max_capacity(capacity)
                .time_to_idle(tti)
                .build(),
            cfg,
        }
    }

    #[cfg(test)]
    pub fn with_capacity_for_test(capacity: u64) -> Self {
        Self::with_capacity_and_tti(AdaptiveCfg::default(), capacity, DEFAULT_ADAPTIVE_TTI)
    }

    #[cfg(test)]
    pub fn sync_for_test(&self) {
        self.state.run_pending_tasks();
        self.limiters.run_pending_tasks();
    }

    #[cfg(test)]
    pub fn state_len_for_test(&self) -> usize {
        self.state.run_pending_tasks();
        self.state.entry_count() as usize
    }

    #[cfg(test)]
    pub fn limiters_len_for_test(&self) -> usize {
        self.limiters.run_pending_tasks();
        self.limiters.entry_count() as usize
    }

    /// Called when a 429 response is received from upstream vendor.
    ///
    /// `header_limit` may contain the upstream `x-ratelimit-limit` header value.
    /// `current_local_rpm` is the current request rate from this gateway instance.
    /// `retry_after_secs` carries the parsed `Retry-After` header when present;
    /// a non-`None` value uses the faster `min_header_observations` threshold.
    pub fn observe_429(
        &self,
        key_id: &KeyId,
        configured_rpm: u32,
        header_limit: Option<u32>,
        current_local_rpm: u32,
        retry_after_secs: Option<u32>,
    ) {
        self.observe_429_with_kind(
            key_id,
            RateLimitKind::Unknown,
            configured_rpm,
            header_limit,
            current_local_rpm,
            retry_after_secs,
        );
    }

    pub fn observe_429_with_kind(
        &self,
        key_id: &KeyId,
        kind: RateLimitKind,
        configured_rpm: u32,
        header_limit: Option<u32>,
        current_local_rpm: u32,
        retry_after_secs: Option<u32>,
    ) {
        if !matches!(kind, RateLimitKind::Rpm | RateLimitKind::Unknown) {
            metrics::counter!(
                "gw_adaptive_rpm_observe_skipped_total",
                "kind" => rate_limit_kind_label(kind),
            )
            .increment(1);
            return;
        }
        metrics::counter!("gw_adaptive_rpm_observe_total").increment(1);
        let now = now_ms();
        let observed_limit = header_limit.unwrap_or(current_local_rpm);
        let required_observations = if header_limit.is_some() || retry_after_secs.is_some() {
            self.cfg.min_header_observations
        } else {
            self.cfg.min_no_header_observations
        };

        let new_state = match self.state.get(key_id) {
            Some(existing) => {
                let mut state = Arc::unwrap_or_clone(existing);
                state.last_429_at_ms = now;
                state.record_observation(observed_limit);
                state.learn_if_ready(observed_limit, required_observations, now);
                set_adaptive_state_metrics(&state);
                Arc::new(state)
            }
            None => {
                let mut state = KeyAdaptiveState::new(configured_rpm);
                state.last_429_at_ms = now;
                state.record_observation(observed_limit);
                state.learn_if_ready(observed_limit, required_observations, now);
                set_adaptive_state_metrics(&state);
                Arc::new(state)
            }
        };
        self.state.insert(key_id.clone(), new_state);
    }

    /// Returns the enforced RPM ceiling if confidence is above threshold.
    /// Returns None if confidence is too low or no learned_rpm exists.
    ///
    /// Pure query (no metric side-effect — the enforcement counter fires in
    /// [`Self::try_admit`] only when a send is actually denied). Clamped to
    /// at least 1 so the GCRA `Quota` is always constructible.
    pub fn should_enforce(&self, key_id: &KeyId) -> Option<u32> {
        let state = self.state.get(key_id)?;
        if state.confidence.value < self.cfg.enforcement_threshold {
            return None;
        }
        let learned_rpm = state.learned_rpm?;
        Some(((learned_rpm as f32 * self.cfg.buffer_ratio) as u32).max(1))
    }

    /// Admission check for sending a request to `key_id`, enforcing the learned
    /// adaptive ceiling via a per-key GCRA (audit 2026-05-30 §1.8). Returns:
    /// - `true` (admit, consuming one cell) when no ceiling is learned yet, or
    ///   the key is within its learned rate;
    /// - `false` (deny, NO cell consumed) when the key is over its learned rate.
    ///
    /// The caller diverts a denied key to another candidate (failover) and only
    /// degrades to sending anyway when every candidate is over its ceiling — the
    /// base per-user RPM bucket and response-side cooldown remain in force, so a
    /// denial is back-pressure, not a hard rejection. The limiter is cached per
    /// key and rebuilt only when the enforced limit changes.
    pub fn try_admit(&self, key_id: &KeyId) -> bool {
        let Some(limit) = self.should_enforce(key_id) else {
            // Nothing learned with enough confidence — admit unconditionally.
            return true;
        };
        // Check if the cached limiter matches the current limit; rebuild if not.
        let limiter = match self.limiters.get(key_id) {
            Some((cached_limit, lim)) if cached_limit == limit => lim,
            _ => {
                // `limit` is clamped >= 1 by should_enforce, so NonZeroU32 is safe.
                let quota = Quota::per_minute(NonZeroU32::new(limit).unwrap_or(NonZeroU32::MIN));
                let lim = Arc::new(RateLimiter::direct(quota));
                self.limiters.insert(key_id.clone(), (limit, lim.clone()));
                lim
            }
        };
        match limiter.check() {
            Ok(()) => true,
            Err(_) => {
                // Over the learned ceiling — this is the real "we enforced" event.
                metrics::counter!("gw_adaptive_rpm_enforced_total").increment(1);
                false
            }
        }
    }

    pub fn tick_decay(&self) {
        let now = now_ms();
        let probe_after_ms = self.cfg.probe_after_min.saturating_mul(60_000);

        // Collect keys that need updating, then re-insert modified states.
        let updates: Vec<(KeyId, Arc<KeyAdaptiveState>)> = self
            .state
            .iter()
            .filter_map(|(key, state)| {
                let elapsed_ms = now.saturating_sub(state.confidence.last_decay_at_ms);
                if elapsed_ms < 60_000 {
                    return None;
                }

                let elapsed_min = elapsed_ms as f32 / 60_000.0;
                let new_confidence = (state.confidence.value
                    - (self.cfg.confidence_decay_per_min * elapsed_min))
                    .max(0.0);
                let was_enforced = state.confidence.value >= self.cfg.enforcement_threshold
                    && state.learned_rpm.is_some();

                let mut new_state = Arc::unwrap_or_clone(Arc::clone(&state));
                new_state.confidence = Confidence::new(new_confidence, now);

                if new_confidence < self.cfg.enforcement_threshold
                    && now.saturating_sub(new_state.last_429_at_ms) >= probe_after_ms
                {
                    if let Some(learned_rpm) = new_state.learned_rpm {
                        new_state.learned_rpm = Some(
                            learned_rpm
                                .saturating_add(self.cfg.probe_increment)
                                .min(self.cfg.max_rpm),
                        );
                        metrics::counter!("gw_adaptive_rpm_probe_total").increment(1);
                    }
                }

                let is_enforced = new_state.confidence.value >= self.cfg.enforcement_threshold
                    && new_state.learned_rpm.is_some();
                if was_enforced && !is_enforced {
                    metrics::counter!("gw_adaptive_rpm_released_total").increment(1);
                }
                set_adaptive_state_metrics(&new_state);

                Some(((*key).clone(), Arc::new(new_state)))
            })
            .collect();

        for (key, state) in updates {
            self.state.insert(key, state);
        }
    }

    pub fn evict(&self, key_id: &KeyId) {
        self.state.invalidate(key_id);
        // Also drop the cached limiter so an evicted key doesn't leave a stale
        // RateLimiter in the cache forever (audit 2026-05-30 §1.8 mem-leak fix).
        self.limiters.invalidate(key_id);
    }
}

fn rate_limit_kind_label(kind: RateLimitKind) -> &'static str {
    match kind {
        RateLimitKind::Rpm => "rpm",
        RateLimitKind::Tpm => "tpm",
        RateLimitKind::Daily => "daily",
        RateLimitKind::Monthly => "monthly",
        RateLimitKind::Concurrent => "concurrent",
        RateLimitKind::Unknown => "unknown",
        _ => "unknown",
    }
}

#[derive(Debug, Clone)]
pub struct KeyAdaptiveState {
    pub configured_rpm: u32,
    pub learned_rpm: Option<u32>,
    pub confidence: Confidence,
    pub last_429_at_ms: u64,
    pub consistent_observations: u32,
    pub last_observed_limit: Option<u32>,
}

impl KeyAdaptiveState {
    pub fn new(configured_rpm: u32) -> Self {
        Self {
            configured_rpm,
            learned_rpm: None,
            confidence: Confidence::new(0.0, 0),
            last_429_at_ms: 0,
            consistent_observations: 0,
            last_observed_limit: None,
        }
    }

    fn record_observation(&mut self, observed_limit: u32) {
        if self.last_observed_limit == Some(observed_limit) {
            self.consistent_observations += 1;
        } else {
            self.consistent_observations = 1;
            self.last_observed_limit = Some(observed_limit);
        }
    }

    fn learn_if_ready(&mut self, observed_limit: u32, required_observations: u32, now: u64) {
        if self.consistent_observations >= required_observations {
            self.learned_rpm = Some(observed_limit);
            self.confidence = Confidence::new(1.0, now);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Confidence {
    pub value: f32,
    pub last_decay_at_ms: u64,
}

impl Confidence {
    pub fn new(value: f32, last_decay_at_ms: u64) -> Self {
        Self {
            value: value.clamp(0.0, 1.0),
            last_decay_at_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveCfg {
    pub enforcement_threshold: f32,
    pub confidence_decay_per_min: f32,
    pub min_header_observations: u32,
    pub min_no_header_observations: u32,
    pub buffer_ratio: f32,
    pub probe_after_min: u64,
    pub probe_increment: u32,
    pub min_rpm: u32,
    pub max_rpm: u32,
}

impl Default for AdaptiveCfg {
    fn default() -> Self {
        Self {
            enforcement_threshold: 0.6,
            confidence_decay_per_min: 0.02,
            min_header_observations: 2,
            min_no_header_observations: 3,
            buffer_ratio: 0.85,
            probe_after_min: 30,
            probe_increment: 5,
            min_rpm: 5,
            max_rpm: 10_000,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_state_evicts_via_lru_at_capacity() {
        let manager = AdaptiveRpmManager::with_capacity_for_test(8);
        // Insert past capacity.
        for i in 0..20 {
            let key = KeyId::from(format!("k-{i}"));
            manager.observe_429_with_kind(
                &key,
                RateLimitKind::Rpm,
                100,
                None,
                50,
                None,
            );
        }
        manager.sync_for_test();
        // Confirm bounded.
        assert!(manager.state_len_for_test() <= 8);
        assert!(manager.limiters_len_for_test() <= 8);
    }
}
