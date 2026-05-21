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
}
