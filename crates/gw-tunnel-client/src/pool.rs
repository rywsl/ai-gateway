//! Connection-pool config + jittered exponential backoff.
//!
//! T3-G owns the actual reconnect loop on top of these primitives. The
//! jittered backoff is exposed as a free fn so transport code and tests can
//! reuse it without paying for an Arc<PoolConfig>.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub heartbeat_interval: Duration,
    pub max_streams_per_conn: u32,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// Fraction of base delay to randomize +/-. Bounded by [0.0, 0.5].
    pub backoff_jitter: f32,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(30),
            max_streams_per_conn: 256,
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            backoff_jitter: 0.2,
        }
    }
}

/// Compute the next backoff: `min * 2^attempt`, capped at `max`, then perturbed
/// by +/- `jitter` fraction.
pub fn jittered_backoff(attempt: u32, cfg: &PoolConfig) -> Duration {
    let base_ms = cfg.backoff_min.as_millis() as f64 * 2_f64.powi(attempt.min(30) as i32);
    let capped = base_ms.min(cfg.backoff_max.as_millis() as f64);
    let jitter = (rand::random::<f32>() - 0.5) * 2.0 * cfg.backoff_jitter;
    let final_ms = capped * (1.0 + jitter as f64);
    Duration::from_millis(final_ms.max(0.0) as u64)
}
