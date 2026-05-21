use gw_proxy::resilience::{RedisHealthState, ResilienceState};

#[test]
fn default_is_healthy() {
    let r = ResilienceState::default();
    assert_eq!(r.redis_state(), RedisHealthState::Healthy);
}

#[test]
fn record_redis_error_degrades() {
    let r = ResilienceState::default();
    r.record_redis_error("get", "timeout");
    assert_eq!(r.redis_state(), RedisHealthState::Degraded);
    assert!(r.last_redis_error_ms() > 0);
}

#[test]
fn record_redis_ok_recovers() {
    let r = ResilienceState::default();
    r.record_redis_error("get", "timeout");
    r.record_redis_ok();
    assert_eq!(r.redis_state(), RedisHealthState::Healthy);
}

#[test]
fn fallback_counters_increment() {
    let r = ResilienceState::default();
    r.bump_freeze_fallback();
    r.bump_cooldown_fallback();
    r.bump_freeze_fallback();
    assert_eq!(r.freeze_fallback_used(), 2);
    assert_eq!(r.cooldown_fallback_used(), 1);
}
