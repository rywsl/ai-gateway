use gw_auth::ratelimit::adaptive::{AdaptiveCfg, AdaptiveRpmManager, Confidence, KeyAdaptiveState};
use gw_core::ids::KeyId;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn test_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[test]
fn test_adaptive_cfg_defaults() {
    let cfg = AdaptiveCfg::default();

    assert_eq!(cfg.enforcement_threshold, 0.6);
    assert_eq!(cfg.confidence_decay_per_min, 0.02);
    assert_eq!(cfg.min_header_observations, 2);
    assert_eq!(cfg.min_no_header_observations, 3);
    assert_eq!(cfg.buffer_ratio, 0.85);
    assert_eq!(cfg.probe_after_min, 30);
    assert_eq!(cfg.probe_increment, 5);
    assert_eq!(cfg.min_rpm, 5);
    assert_eq!(cfg.max_rpm, 10_000);
}

#[test]
fn test_key_adaptive_state_initial() {
    let state = KeyAdaptiveState::new(120);

    assert_eq!(state.configured_rpm, 120);
    assert_eq!(state.learned_rpm, None);
    assert_eq!(state.confidence.value, 0.0);
    assert_eq!(state.confidence.last_decay_at_ms, 0);
    assert_eq!(state.last_429_at_ms, 0);
    assert_eq!(state.consistent_observations, 0);
    assert_eq!(state.last_observed_limit, None);
}

#[test]
fn test_confidence_clamp() {
    let below = Confidence::new(-0.25, 10);
    let inside = Confidence::new(0.4, 20);
    let above = Confidence::new(1.25, 30);

    assert_eq!(below.value, 0.0);
    assert_eq!(below.last_decay_at_ms, 10);
    assert_eq!(inside.value, 0.4);
    assert_eq!(inside.last_decay_at_ms, 20);
    assert_eq!(above.value, 1.0);
    assert_eq!(above.last_decay_at_ms, 30);
}

#[test]
fn test_manager_new_empty() {
    let cfg = AdaptiveCfg::default();
    let manager = AdaptiveRpmManager::new(cfg);

    assert!(manager.state.is_empty());
    assert_eq!(manager.state.len(), 0);
    assert_eq!(manager.cfg.min_rpm, 5);
    assert!(manager.state.get(&KeyId::from("missing-key")).is_none());
}

#[test]
fn test_observe_429_with_header_learns_after_2() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg::default());
    let key_id = KeyId::from("vendor-key-header");

    manager.observe_429(&key_id, 120, Some(60), 120, None);
    assert_eq!(manager.state.get(&key_id).unwrap().learned_rpm, None);

    manager.observe_429(&key_id, 120, Some(60), 120, None);
    let state = manager.state.get(&key_id).unwrap();

    assert_eq!(state.configured_rpm, 120);
    assert_eq!(state.learned_rpm, Some(60));
    assert_eq!(state.confidence.value, 1.0);
    assert_eq!(state.consistent_observations, 2);
    assert_eq!(state.last_observed_limit, Some(60));
    assert!(state.last_429_at_ms > 0);
}

#[test]
fn test_observe_429_without_header_learns_after_3() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg::default());
    let key_id = KeyId::from("vendor-key-no-header");

    manager.observe_429(&key_id, 240, None, 90, None);
    manager.observe_429(&key_id, 240, None, 90, None);
    assert_eq!(manager.state.get(&key_id).unwrap().learned_rpm, None);

    manager.observe_429(&key_id, 240, None, 90, None);
    let state = manager.state.get(&key_id).unwrap();

    assert_eq!(state.learned_rpm, Some(90));
    assert_eq!(state.confidence.value, 1.0);
    assert_eq!(state.consistent_observations, 3);
    assert_eq!(state.last_observed_limit, Some(90));
}

#[test]
fn test_observe_429_inconsistent_resets() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg::default());
    let key_id = KeyId::from("vendor-key-inconsistent");

    manager.observe_429(&key_id, 120, Some(60), 120, None);
    manager.observe_429(&key_id, 120, Some(75), 120, None);
    let state = manager.state.get(&key_id).unwrap();

    assert_eq!(state.learned_rpm, None);
    assert_eq!(state.confidence.value, 0.0);
    assert_eq!(state.consistent_observations, 1);
    assert_eq!(state.last_observed_limit, Some(75));
}

#[test]
fn test_aimd_multiplicative_decrease() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg::default());
    let key_id = KeyId::from("vendor-key-aimd");

    manager.observe_429(&key_id, 120, Some(60), 120, None);
    manager.observe_429(&key_id, 120, Some(60), 120, None);

    assert_eq!(manager.should_enforce(&key_id), Some(51));
}

#[test]
fn test_enforcement_below_threshold_skips() {
    let cfg = AdaptiveCfg {
        min_header_observations: 1,
        enforcement_threshold: 1.1,
        ..AdaptiveCfg::default()
    };
    let manager = AdaptiveRpmManager::new(cfg);
    let key_id = KeyId::from("vendor-key-below-threshold");

    manager.observe_429(&key_id, 120, Some(60), 120, None);

    assert_eq!(manager.state.get(&key_id).unwrap().learned_rpm, Some(60));
    assert_eq!(manager.should_enforce(&key_id), None);
}

#[test]
fn test_enforcement_above_threshold_enforces() {
    let cfg = AdaptiveCfg {
        min_header_observations: 1,
        enforcement_threshold: 0.6,
        ..AdaptiveCfg::default()
    };
    let manager = AdaptiveRpmManager::new(cfg);
    let key_id = KeyId::from("vendor-key-above-threshold");

    manager.observe_429(&key_id, 120, Some(80), 120, None);

    assert_eq!(manager.should_enforce(&key_id), Some(68));
}

#[test]
fn test_tick_decay_reduces_confidence() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg::default());
    let key_id = KeyId::from("vendor-key-decay");
    let now = test_now_ms();
    manager.state.insert(
        key_id.clone(),
        Arc::new(KeyAdaptiveState {
            configured_rpm: 120,
            learned_rpm: Some(80),
            confidence: Confidence::new(1.0, now.saturating_sub(60_000)),
            last_429_at_ms: now,
            consistent_observations: 2,
            last_observed_limit: Some(80),
        }),
    );

    manager.tick_decay();

    let state = manager.state.get(&key_id).unwrap();
    assert!((state.confidence.value - 0.98).abs() < 0.001);
}

#[test]
fn test_tick_decay_probe_expansion() {
    let cfg = AdaptiveCfg {
        enforcement_threshold: 0.99,
        ..AdaptiveCfg::default()
    };
    let manager = AdaptiveRpmManager::new(cfg);
    let key_id = KeyId::from("vendor-key-probe");
    let now = test_now_ms();
    manager.state.insert(
        key_id.clone(),
        Arc::new(KeyAdaptiveState {
            configured_rpm: 120,
            learned_rpm: Some(80),
            confidence: Confidence::new(1.0, now.saturating_sub(60_000)),
            last_429_at_ms: now.saturating_sub(31 * 60_000),
            consistent_observations: 2,
            last_observed_limit: Some(80),
        }),
    );

    manager.tick_decay();

    let state = manager.state.get(&key_id).unwrap();
    assert_eq!(state.learned_rpm, Some(85));
    assert_eq!(manager.should_enforce(&key_id), None);
}

#[test]
fn test_tick_decay_below_zero_clamps() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg::default());
    let key_id = KeyId::from("vendor-key-zero-clamp");
    let now = test_now_ms();
    manager.state.insert(
        key_id.clone(),
        Arc::new(KeyAdaptiveState {
            configured_rpm: 120,
            learned_rpm: Some(80),
            confidence: Confidence::new(0.01, now.saturating_sub(60_000)),
            last_429_at_ms: now,
            consistent_observations: 2,
            last_observed_limit: Some(80),
        }),
    );

    manager.tick_decay();

    let state = manager.state.get(&key_id).unwrap();
    assert_eq!(state.confidence.value, 0.0);
}

#[test]
fn test_evict_removes_key() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg::default());
    let key_id = KeyId::from("vendor-key-evict");
    manager.observe_429(&key_id, 120, Some(80), 120, None);
    manager.observe_429(&key_id, 120, Some(80), 120, None);
    assert_eq!(manager.should_enforce(&key_id), Some(68));

    manager.evict(&key_id);

    assert_eq!(manager.should_enforce(&key_id), None);
    assert!(manager.state.get(&key_id).is_none());
}

#[test]
fn test_probe_respects_max_rpm() {
    let cfg = AdaptiveCfg {
        max_rpm: 82,
        enforcement_threshold: 0.99,
        ..AdaptiveCfg::default()
    };
    let manager = AdaptiveRpmManager::new(cfg);
    let key_id = KeyId::from("vendor-key-probe-cap");
    let now = test_now_ms();
    manager.state.insert(
        key_id.clone(),
        Arc::new(KeyAdaptiveState {
            configured_rpm: 120,
            learned_rpm: Some(80),
            confidence: Confidence::new(1.0, now.saturating_sub(60_000)),
            last_429_at_ms: now.saturating_sub(31 * 60_000),
            consistent_observations: 2,
            last_observed_limit: Some(80),
        }),
    );

    manager.tick_decay();

    let state = manager.state.get(&key_id).unwrap();
    assert_eq!(state.learned_rpm, Some(82));
}

#[test]
fn test_observe_429_retry_after_uses_header_threshold() {
    let cfg = AdaptiveCfg {
        min_header_observations: 2,
        min_no_header_observations: 3,
        ..AdaptiveCfg::default()
    };
    let manager = AdaptiveRpmManager::new(cfg);
    let key_id = KeyId::from("vendor-key-retry-after");

    manager.observe_429(&key_id, 240, None, 90, Some(30));
    assert_eq!(manager.state.get(&key_id).unwrap().learned_rpm, None);

    manager.observe_429(&key_id, 240, None, 90, Some(30));
    let state = manager.state.get(&key_id).unwrap();
    assert_eq!(state.learned_rpm, Some(90));
    assert_eq!(state.confidence.value, 1.0);
}

#[test]
fn test_observe_429_no_retry_after_needs_3_observations() {
    let cfg = AdaptiveCfg {
        min_header_observations: 2,
        min_no_header_observations: 3,
        ..AdaptiveCfg::default()
    };
    let manager = AdaptiveRpmManager::new(cfg);
    let key_id = KeyId::from("vendor-key-no-retry-after");

    manager.observe_429(&key_id, 240, None, 90, None);
    manager.observe_429(&key_id, 240, None, 90, None);
    assert_eq!(manager.state.get(&key_id).unwrap().learned_rpm, None);

    manager.observe_429(&key_id, 240, None, 90, None);
    assert_eq!(manager.state.get(&key_id).unwrap().learned_rpm, Some(90));
}

// Audit 2026-05-30 §1.8: try_admit is the live enforcement of the learned
// ceiling (the former request_filter clamp was dead code). pick_peer uses it.
#[test]
fn test_try_admit_enforces_learned_ceiling() {
    let manager = AdaptiveRpmManager::new(AdaptiveCfg::default());
    let key = KeyId::from("vk-admit");

    // No learned ceiling yet → admit unconditionally (no limiter built).
    assert!(manager.try_admit(&key), "no ceiling → admit");

    // Learn a 1-rpm ceiling (2 header observations). enforced = (1*0.85).max(1) = 1.
    manager.observe_429(&key, 120, Some(1), 120, None);
    manager.observe_429(&key, 120, Some(1), 120, None);
    assert_eq!(manager.should_enforce(&key), Some(1));

    // GCRA burst for per_minute(1) is 1: first send admits, the next is denied.
    assert!(manager.try_admit(&key), "first send within ceiling");
    assert!(!manager.try_admit(&key), "second send over the 1-rpm ceiling");

    // Evicting the key clears both state and the cached limiter, so admission
    // is unconditional again (no stale limiter — mem-leak guard).
    manager.evict(&key);
    assert!(manager.try_admit(&key), "post-evict → admit (limiter cleared)");
}
