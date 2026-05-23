//! Resilience state — tracks Redis health and fallback usage counters.
//!
//! Used by ErrorHandler listeners (T3-A) to record degraded states when
//! Redis ops fail, and by freeze / cooldown paths to bump fallback counters
//! when in-process fallbacks are taken (Wave 6 hooks these into filter chain).

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RedisHealthState {
    Healthy = 0,
    Degraded = 1,
    Unavailable = 2,
}

impl From<u8> for RedisHealthState {
    fn from(v: u8) -> Self {
        match v {
            0 => Self::Healthy,
            1 => Self::Degraded,
            _ => Self::Unavailable,
        }
    }
}

#[derive(Default)]
pub struct ResilienceState {
    redis_state: AtomicU8,
    last_redis_error_ms: AtomicU64,
    freeze_fallback_used: AtomicU64,
    cooldown_fallback_used: AtomicU64,
}

impl ResilienceState {
    pub fn redis_state(&self) -> RedisHealthState {
        self.redis_state.load(Ordering::Acquire).into()
    }

    pub fn last_redis_error_ms(&self) -> u64 {
        self.last_redis_error_ms.load(Ordering::Acquire)
    }

    pub fn record_redis_error(&self, _op: &str, _reason: &str) {
        self.redis_state
            .store(RedisHealthState::Degraded as u8, Ordering::Release);
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.last_redis_error_ms.store(ms, Ordering::Release);
        metrics::counter!("gw_resilience_redis_error_total").increment(1);
    }

    pub fn record_redis_ok(&self) {
        self.redis_state
            .store(RedisHealthState::Healthy as u8, Ordering::Release);
    }

    pub fn bump_freeze_fallback(&self) {
        self.freeze_fallback_used.fetch_add(1, Ordering::AcqRel);
        metrics::counter!("gw_resilience_freeze_fallback_total").increment(1);
    }

    pub fn bump_cooldown_fallback(&self) {
        self.cooldown_fallback_used.fetch_add(1, Ordering::AcqRel);
        metrics::counter!("gw_resilience_cooldown_fallback_total").increment(1);
    }

    pub fn freeze_fallback_used(&self) -> u64 {
        self.freeze_fallback_used.load(Ordering::Acquire)
    }

    pub fn cooldown_fallback_used(&self) -> u64 {
        self.cooldown_fallback_used.load(Ordering::Acquire)
    }

    /// JSON snapshot of the resilience state, intended for the admin
    /// `/healthz/resilience` endpoint (T3-C consumer). Shape:
    ///
    /// ```json
    /// {
    ///   "redis_state": "healthy" | "degraded" | "unavailable",
    ///   "last_redis_error_ms": 0,
    ///   "freeze_fallback_used": 0,
    ///   "cooldown_fallback_used": 0
    /// }
    /// ```
    ///
    /// Observability JSON only — not a wire contract. Field names may
    /// evolve.
    pub fn snapshot_json(&self) -> String {
        let v = serde_json::json!({
            "redis_state": redis_state_label(self.redis_state()),
            "last_redis_error_ms": self.last_redis_error_ms(),
            "freeze_fallback_used": self.freeze_fallback_used(),
            "cooldown_fallback_used": self.cooldown_fallback_used(),
        });
        serde_json::to_string(&v).unwrap_or_else(|_| String::from("{}"))
    }
}

fn redis_state_label(s: RedisHealthState) -> &'static str {
    match s {
        RedisHealthState::Healthy => "healthy",
        RedisHealthState::Degraded => "degraded",
        RedisHealthState::Unavailable => "unavailable",
    }
}

#[cfg(test)]
mod resilience_snapshot_tests {
    use super::*;

    #[test]
    fn snapshot_json_emits_required_fields() {
        let state = ResilienceState::default();
        let json = state.snapshot_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_object());
        assert!(parsed.get("redis_state").is_some(), "redis_state missing");
        assert!(
            parsed.get("last_redis_error_ms").is_some(),
            "last_redis_error_ms missing"
        );
        assert!(
            parsed.get("freeze_fallback_used").is_some(),
            "freeze_fallback_used missing"
        );
        assert!(
            parsed.get("cooldown_fallback_used").is_some(),
            "cooldown_fallback_used missing"
        );
        // Default state is healthy.
        assert_eq!(parsed["redis_state"], "healthy");
    }

    #[test]
    fn snapshot_reflects_recorded_redis_error() {
        let state = ResilienceState::default();
        state.record_redis_error("get", "timeout");
        let json = state.snapshot_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["redis_state"], "degraded");
        assert!(parsed["last_redis_error_ms"].as_u64().unwrap_or(0) > 0);
    }
}
