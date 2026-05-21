use gw_tunnel_client::pool::{jittered_backoff, PoolConfig};
use std::time::Duration;

#[test]
fn backoff_grows_exponentially_capped_at_max() {
    let cfg = PoolConfig::default();
    let b1 = jittered_backoff(1, &cfg);
    let b3 = jittered_backoff(3, &cfg);
    let b10 = jittered_backoff(10, &cfg);
    assert!(b3 >= b1.mul_f32(0.5));
    assert!(b10 <= cfg.backoff_max.mul_f32(1.25));
    assert!(b10 >= cfg.backoff_max.mul_f32(0.75));
}

#[test]
fn default_pool_config_has_reasonable_values() {
    let cfg = PoolConfig::default();
    assert_eq!(cfg.heartbeat_interval, Duration::from_secs(30));
    assert!(cfg.max_streams_per_conn >= 64);
    assert!(cfg.backoff_min < cfg.backoff_max);
    assert!(cfg.backoff_jitter >= 0.0 && cfg.backoff_jitter <= 0.5);
}
