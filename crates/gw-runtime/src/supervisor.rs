//! Bounded-restart supervisor for long-running tasks.
//!
//! Wraps a task factory in a restart loop:
//! - Each invocation catches `Err` and `panic` (via `catch_unwind`).
//! - Backoff between restarts is `backoff_initial`, scaled by
//!   `backoff_multiplier` up to `backoff_max`, with `backoff_jitter`
//!   applied symmetrically.
//! - If more than `max_restarts` failures occur within `window`, the task
//!   is marked unhealthy and the loop exits.
//! - `ShutdownToken` cancellation breaks the loop immediately.

use crate::shutdown::ShutdownToken;
use futures::FutureExt;
use std::collections::VecDeque;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::task::JoinHandle;

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("task returned error: {0}")]
    Task(#[from] anyhow::Error),
}

#[derive(Clone, Debug)]
pub struct RestartPolicy {
    pub max_restarts: u32,
    pub window: Duration,
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    pub backoff_multiplier: f64,
    pub backoff_jitter: f64, // 0.0 .. 1.0
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            window: Duration::from_secs(60),
            backoff_initial: Duration::from_millis(100),
            backoff_max: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            backoff_jitter: 0.2,
        }
    }
}

pub struct Supervisor {
    name: &'static str,
    policy: RestartPolicy,
    token: ShutdownToken,
    unhealthy: Arc<AtomicBool>,
}

impl Supervisor {
    pub fn new(name: &'static str, policy: RestartPolicy, token: ShutdownToken) -> Self {
        Self {
            name,
            policy,
            token,
            unhealthy: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn unhealthy_flag(&self) -> Arc<AtomicBool> {
        self.unhealthy.clone()
    }

    #[must_use = "supervisor handle should be awaited or tracked"]
    pub fn spawn<F, Fut>(self, factory: F) -> JoinHandle<()>
    where
        F: Fn(ShutdownToken) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), anyhow::Error>> + Send + 'static,
    {
        let Self {
            name,
            policy,
            token,
            unhealthy,
        } = self;
        tokio::spawn(async move {
            let mut failures: VecDeque<Instant> = VecDeque::new();
            let mut backoff = policy.backoff_initial;

            loop {
                if token.is_cancelled() {
                    tracing::info!(target: "gw_runtime::supervisor", task = name, "shutdown requested");
                    break;
                }

                let child_token = token.child();
                // Build the future *inside* the catch_unwind wrapper so a
                // panic during closure body / future construction is also
                // caught, not just panics inside the polled future.
                let result = AssertUnwindSafe(async { factory(child_token).await })
                    .catch_unwind()
                    .await;

                let outcome = match result {
                    Ok(Ok(())) => {
                        tracing::info!(target: "gw_runtime::supervisor", task = name, "task exited cleanly");
                        if token.is_cancelled() {
                            break;
                        }
                        // Clean exit is not a failure: do not record it in
                        // the failure window, reset backoff, skip the
                        // restart metric, and re-enter the loop without
                        // sleeping.
                        backoff = policy.backoff_initial;
                        continue;
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(target: "gw_runtime::supervisor", task = name, error = %err, "task returned error");
                        "error"
                    }
                    Err(panic) => {
                        let payload = panic_payload_string(&panic);
                        tracing::error!(target: "gw_runtime::supervisor", task = name, panic = %payload, "task panicked");
                        "panic"
                    }
                };

                metrics::counter!("gw_supervisor_restart_total", "task" => name, "reason" => outcome).increment(1);

                let now = Instant::now();
                failures.push_back(now);
                while let Some(front) = failures.front() {
                    if now.duration_since(*front) > policy.window {
                        failures.pop_front();
                    } else {
                        break;
                    }
                }

                if failures.len() as u32 > policy.max_restarts {
                    tracing::error!(
                        target: "gw_runtime::supervisor",
                        task = name,
                        failures = failures.len(),
                        "exceeded restart budget; marking unhealthy and giving up"
                    );
                    unhealthy.store(true, Ordering::SeqCst);
                    metrics::gauge!("gw_supervisor_unhealthy", "task" => name).set(1.0);
                    break;
                }

                // Apply backoff with token-aware sleep.
                let delay = apply_jitter(backoff, policy.backoff_jitter);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = token.cancelled() => break,
                }
                backoff = next_backoff(backoff, &policy);
            }
        })
    }
}

fn next_backoff(current: Duration, policy: &RestartPolicy) -> Duration {
    let next = current.mul_f64(policy.backoff_multiplier);
    if next > policy.backoff_max {
        policy.backoff_max
    } else {
        next
    }
}

fn apply_jitter(d: Duration, jitter: f64) -> Duration {
    if jitter <= 0.0 {
        return d;
    }
    // Seed jitter from the wall clock sub-second nanos. `Instant::now()`
    // is too high-resolution and varies by sub-microsecond between calls,
    // which made every call land in nearly the same bucket; `SystemTime`'s
    // sub-second component spreads across the full 0..1 range across
    // restart attempts.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    let frac = (nanos % 1_000) as f64 / 1_000.0; // 0.0..1.0
    let signed = (frac - 0.5) * 2.0 * jitter; // -jitter..+jitter
    let scaled = d.as_secs_f64() * (1.0 + signed);
    Duration::from_secs_f64(scaled.max(0.0))
}

fn panic_payload_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    }
}
