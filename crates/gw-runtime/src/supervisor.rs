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
use once_cell::sync::Lazy;
use std::collections::{HashMap, VecDeque};
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

#[must_use = "one-shot task handle should be awaited or tracked"]
pub fn one_shot_spawn<F, Fut>(name: &'static str, fut: F) -> JoinHandle<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), anyhow::Error>> + Send + 'static,
{
    let flag = SUPERVISOR_REGISTRY.register(name);
    tokio::spawn(async move {
        let result = AssertUnwindSafe(async { fut().await }).catch_unwind().await;
        match &result {
            Ok(Ok(())) => {
                metrics::counter!("gw_oneshot_total", "task" => name, "result" => "success")
                    .increment(1);
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    target: "gw_runtime::oneshot",
                    task = name,
                    error = %err,
                    "one-shot returned error"
                );
                flag.store(true, Ordering::SeqCst);
                metrics::gauge!("gw_supervisor_unhealthy", "task" => name).set(1.0);
                metrics::counter!("gw_oneshot_total", "task" => name, "result" => "error")
                    .increment(1);
            }
            Err(panic) => {
                let payload = panic_payload_string(panic);
                tracing::error!(
                    target: "gw_runtime::oneshot",
                    task = name,
                    panic = %payload,
                    "one-shot panicked"
                );
                flag.store(true, Ordering::SeqCst);
                metrics::gauge!("gw_supervisor_unhealthy", "task" => name).set(1.0);
                metrics::counter!("gw_oneshot_total", "task" => name, "result" => "panic")
                    .increment(1);
            }
        }
    })
}

// --- SupervisorRegistry -----------------------------------------------------

/// Process-wide registry of supervised task health flags.
///
/// Each `supervised_spawn`/`one_shot_spawn` task registers an
/// `Arc<AtomicBool>` here. The flag is `false` while healthy and flips to
/// `true` when the task burns its restart budget or returns/panics
/// irrecoverably. `/readyz` reads `all_healthy()` to decide 200 vs 503.
pub struct SupervisorRegistry {
    flags: parking_lot::RwLock<HashMap<&'static str, Arc<AtomicBool>>>,
}

impl SupervisorRegistry {
    pub fn new() -> Self {
        Self {
            flags: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    /// Register a task and return its health flag. The flag starts healthy
    /// (`false`). Re-registering the same name returns the existing flag so
    /// idempotent boot paths do not panic.
    pub fn register(&self, name: &'static str) -> Arc<AtomicBool> {
        let mut guard = self.flags.write();
        if let Some(existing) = guard.get(&name) {
            return Arc::clone(existing);
        }
        let flag = Arc::new(AtomicBool::new(false));
        guard.insert(name, Arc::clone(&flag));
        flag
    }

    /// Register a task with an externally-owned flag (e.g. the one created by
    /// `Supervisor::new`). The registry and the caller share the same
    /// `Arc<AtomicBool>` so a flip is immediately visible to `/readyz`.
    pub fn register_with_flag(&self, name: &'static str, flag: Arc<AtomicBool>) {
        let mut guard = self.flags.write();
        guard.insert(name, flag);
    }

    /// `true` when every registered task is healthy.
    pub fn all_healthy(&self) -> bool {
        self.flags
            .read()
            .values()
            .all(|f| !f.load(Ordering::SeqCst))
    }

    /// Sorted list of task names whose health flag has flipped to unhealthy.
    pub fn unhealthy_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .flags
            .read()
            .iter()
            .filter(|(_, f)| f.load(Ordering::SeqCst))
            .map(|(n, _)| (*n).to_string())
            .collect();
        v.sort_unstable();
        v
    }

    /// Test-only: clear all registrations so unit tests start from a clean
    /// registry. Production code never calls this.
    #[doc(hidden)]
    pub fn reset_for_tests(&self) {
        self.flags.write().clear();
    }
}

impl Default for SupervisorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub static SUPERVISOR_REGISTRY: Lazy<SupervisorRegistry> = Lazy::new(SupervisorRegistry::new);

/// Spawn a supervised task that is registered with `SUPERVISOR_REGISTRY`.
///
/// Thin wrapper over `Supervisor::new(...).spawn(factory)` that also shares
/// the supervisor's `unhealthy` flag with the process-wide registry so
/// `/readyz` can aggregate. The factory receives a child `ShutdownToken` so
/// the task can observe cancellation.
#[must_use = "supervised task handle should be awaited or tracked"]
pub fn supervised_spawn<F, Fut>(
    name: &'static str,
    policy: RestartPolicy,
    token: ShutdownToken,
    factory: F,
) -> JoinHandle<()>
where
    F: Fn(ShutdownToken) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), anyhow::Error>> + Send + 'static,
{
    let sup = Supervisor::new(name, policy, token);
    let flag = sup.unhealthy_flag();
    // Share the supervisor's unhealthy flag with the registry so /readyz
    // sees the flip immediately when the supervisor burns its budget — no
    // watchdog task needed.
    SUPERVISOR_REGISTRY.register_with_flag(name, flag);
    sup.spawn(factory)
}

// --- Policy constructors ----------------------------------------------------

pub fn policy_default() -> RestartPolicy {
    RestartPolicy::default()
}

pub fn policy_archive() -> RestartPolicy {
    RestartPolicy {
        max_restarts: 10,
        window: Duration::from_secs(60),
        backoff_initial: Duration::from_millis(100),
        backoff_max: Duration::from_secs(5),
        backoff_multiplier: 2.0,
        backoff_jitter: 0.2,
    }
}

pub fn policy_probe() -> RestartPolicy {
    RestartPolicy {
        max_restarts: 5,
        window: Duration::from_secs(300),
        backoff_initial: Duration::from_secs(1),
        backoff_max: Duration::from_secs(30),
        backoff_multiplier: 2.0,
        backoff_jitter: 0.2,
    }
}

pub fn policy_heartbeat() -> RestartPolicy {
    RestartPolicy {
        max_restarts: 10,
        window: Duration::from_secs(120),
        backoff_initial: Duration::from_secs(1),
        backoff_max: Duration::from_secs(10),
        backoff_multiplier: 2.0,
        backoff_jitter: 0.2,
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
