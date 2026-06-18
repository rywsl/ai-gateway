//! Retry primitive for Redis and other short idempotent control-plane operations.

use rand::Rng;
use std::future::Future;
use std::time::Duration;
use tokio::time::Instant;

use crate::ShutdownToken;

#[derive(Clone, Debug)]
pub struct RetryConfig {
    pub op_name: String,
    pub max_retries: usize,
    pub initial_backoff: Duration,
    pub multipliers: Vec<f64>,
    pub jitter: f64,
    pub total_deadline: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            op_name: "unknown".to_string(),
            max_retries: 5,
            initial_backoff: Duration::from_millis(100),
            multipliers: vec![1.0, 1.0, 2.0, 4.0, 8.0],
            jitter: 0.2,
            total_deadline: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum RetryError<E> {
    Cancelled,
    Exhausted(E),
}

pub async fn retry<F, Fut, T, E>(cfg: &RetryConfig, mut f: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Debug,
{
    let started = Instant::now();
    let mut last_err = None;

    for attempt in 0..=cfg.max_retries {
        if attempt > 0 {
            let elapsed = started.elapsed();
            if elapsed >= cfg.total_deadline {
                return exhausted(cfg, attempt, last_err);
            }

            let sleep_for = backoff_for(cfg, attempt).min(cfg.total_deadline - elapsed);
            tokio::time::sleep(sleep_for).await;

            if started.elapsed() >= cfg.total_deadline {
                return exhausted(cfg, attempt, last_err);
            }
        }

        match f().await {
            Ok(value) => {
                metrics::histogram!("redis_retry_attempts", "op" => cfg.op_name.clone())
                    .record((attempt + 1) as f64);
                if attempt > 0 {
                    tracing::info!(
                        target: "gw_runtime::redis_retry",
                        op = %cfg.op_name,
                        attempts = attempt + 1,
                        "retry succeeded"
                    );
                }
                return Ok(value);
            }
            Err(err) => {
                tracing::warn!(
                    target: "gw_runtime::redis_retry",
                    op = %cfg.op_name,
                    attempt = attempt + 1,
                    error = ?err,
                    remaining_ms = cfg.total_deadline.saturating_sub(started.elapsed()).as_millis(),
                    "retry attempt failed"
                );
                last_err = Some(err);
            }
        }
    }

    exhausted(cfg, cfg.max_retries + 1, last_err)
}

pub async fn retry_with_shutdown<F, Fut, T, E>(
    cfg: &RetryConfig,
    shutdown: &ShutdownToken,
    mut f: F,
) -> Result<T, RetryError<E>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: std::fmt::Debug,
{
    let started = Instant::now();
    let mut last_err = None;

    for attempt in 0..=cfg.max_retries {
        if shutdown.is_cancelled() {
            return Err(RetryError::Cancelled);
        }

        if attempt > 0 {
            let elapsed = started.elapsed();
            if elapsed >= cfg.total_deadline {
                return exhausted_with_retry_error(cfg, attempt, last_err);
            }

            let sleep_for = backoff_for(cfg, attempt).min(cfg.total_deadline - elapsed);
            tokio::select! {
                _ = shutdown.cancelled() => return Err(RetryError::Cancelled),
                _ = tokio::time::sleep(sleep_for) => {}
            }

            if started.elapsed() >= cfg.total_deadline {
                return exhausted_with_retry_error(cfg, attempt, last_err);
            }
        }

        match f().await {
            Ok(value) => {
                metrics::histogram!("redis_retry_attempts", "op" => cfg.op_name.clone())
                    .record((attempt + 1) as f64);
                if attempt > 0 {
                    tracing::info!(
                        target: "gw_runtime::redis_retry",
                        op = %cfg.op_name,
                        attempts = attempt + 1,
                        "retry succeeded"
                    );
                }
                return Ok(value);
            }
            Err(err) => {
                tracing::warn!(
                    target: "gw_runtime::redis_retry",
                    op = %cfg.op_name,
                    attempt = attempt + 1,
                    error = ?err,
                    remaining_ms = cfg.total_deadline.saturating_sub(started.elapsed()).as_millis(),
                    "retry attempt failed"
                );
                last_err = Some(err);
            }
        }
    }

    exhausted_with_retry_error(cfg, cfg.max_retries + 1, last_err)
}

fn exhausted<T, E>(cfg: &RetryConfig, attempts: usize, last_err: Option<E>) -> Result<T, E>
where
    E: std::fmt::Debug,
{
    metrics::counter!("redis_retry_exhausted_total", "op" => cfg.op_name.clone()).increment(1);
    tracing::error!(
        target: "gw_runtime::redis_retry",
        op = %cfg.op_name,
        attempts,
        "retry exhausted"
    );
    Err(last_err.expect("retry exhausted without at least one attempt"))
}

fn exhausted_with_retry_error<T, E>(
    cfg: &RetryConfig,
    attempts: usize,
    last_err: Option<E>,
) -> Result<T, RetryError<E>>
where
    E: std::fmt::Debug,
{
    exhausted(cfg, attempts, last_err).map_err(RetryError::Exhausted)
}

fn backoff_for(cfg: &RetryConfig, attempt: usize) -> Duration {
    let idx = (attempt - 1).min(cfg.multipliers.len().saturating_sub(1));
    let multiplier = cfg.multipliers.get(idx).copied().unwrap_or(1.0).max(0.0);
    let base = cfg.initial_backoff.mul_f64(multiplier);
    apply_jitter(base, cfg.jitter)
}

fn apply_jitter(backoff: Duration, jitter: f64) -> Duration {
    if jitter <= 0.0 || backoff.is_zero() {
        return backoff;
    }
    let bounded = jitter.clamp(0.0, 1.0);
    let delta = rand::thread_rng().gen_range(-bounded..=bounded);
    backoff.mul_f64((1.0 + delta).max(0.0))
}
