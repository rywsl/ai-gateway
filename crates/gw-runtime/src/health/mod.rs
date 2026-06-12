//! Health hooks + system monitor + backpressure gate.
//!
//! Design (see `docs/superpowers/specs/2026-06-11-resilience-fixes-design.md`):
//! - L1 buffers self-heal silently.
//! - L2 metrics observe.
//! - L3 force-majeure hooks (this module) flip a global BackpressureGate.

pub mod disk;
pub mod fd;
pub mod memory;
pub mod redis_oom;

use futures::FutureExt;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HealthSignal {
    Healthy,
    Critical,
}

pub trait HealthHook: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn check(&self) -> HealthSignal;
}

pub struct SystemHealthMonitor {
    hooks: Vec<Arc<dyn HealthHook>>,
    state: Arc<AtomicU8>,          // 0 healthy / 1 critical
    healthy_streak: Arc<AtomicU8>, // consecutive healthy ticks
    hysteresis_ticks: u8,
}

impl SystemHealthMonitor {
    pub fn new(hysteresis_ticks: u8) -> Self {
        Self {
            hooks: Vec::new(),
            state: Arc::new(AtomicU8::new(0)),
            healthy_streak: Arc::new(AtomicU8::new(hysteresis_ticks)),
            hysteresis_ticks,
        }
    }

    pub fn add_hook<H: HealthHook>(&mut self, h: H) -> &mut Self {
        self.hooks.push(Arc::new(h));
        self
    }

    pub fn state_handle(&self) -> Arc<AtomicU8> {
        self.state.clone()
    }

    pub fn evaluate(&self) -> HealthSignal {
        let mut any_crit = None;
        for h in &self.hooks {
            if h.check() == HealthSignal::Critical {
                metrics::gauge!("gw_health_hook_critical", "hook" => h.name()).set(1.0);
                any_crit = Some(h.name());
            } else {
                metrics::gauge!("gw_health_hook_critical", "hook" => h.name()).set(0.0);
            }
        }
        let was_critical = self.state.load(Ordering::Acquire) == 1;
        if let Some(name) = any_crit {
            if !was_critical {
                tracing::warn!(target: "gw_runtime::health", hook = name, "system entered critical state");
                metrics::counter!("gw_health_hook_transition_total", "hook" => name, "to" => "critical").increment(1);
            }
            self.state.store(1, Ordering::Release);
            self.healthy_streak.store(0, Ordering::Release);
            metrics::gauge!("gw_health_state").set(1.0);
            HealthSignal::Critical
        } else {
            let prev_streak = self.healthy_streak.load(Ordering::Acquire);
            let new_streak = prev_streak.saturating_add(1).min(self.hysteresis_ticks);
            self.healthy_streak.store(new_streak, Ordering::Release);
            if was_critical && new_streak >= self.hysteresis_ticks {
                tracing::info!(target: "gw_runtime::health", "system recovered to healthy");
                metrics::counter!("gw_health_recovery_total").increment(1);
                self.state.store(0, Ordering::Release);
            }
            metrics::gauge!("gw_health_state").set(self.state.load(Ordering::Acquire) as f64);
            HealthSignal::Healthy
        }
    }

    /// Spawn the periodic evaluator loop. Returns the JoinHandle to register
    /// with ShutdownCoordinator; callers can wrap this in `Supervisor::spawn`
    /// for restart-on-panic instead of relying on the bare task.
    pub fn into_loop(self, interval: Duration) -> JoinHandle<()> {
        let monitor = Arc::new(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                let monitor = monitor.clone();
                let result = AssertUnwindSafe(async move { monitor.evaluate() })
                    .catch_unwind()
                    .await;
                if let Err(_panic) = result {
                    metrics::counter!("gw_health_monitor_panic_total").increment(1);
                    tracing::error!(target: "gw_runtime::health", "monitor evaluate() panicked; will retry next tick");
                }
            }
        })
    }
}

#[derive(Clone)]
pub struct BackpressureGate {
    state: Arc<AtomicU8>,
}

pub enum Admit {
    Pass,
    Reject,
}

impl BackpressureGate {
    pub fn from_state(state: Arc<AtomicU8>) -> Self {
        Self { state }
    }

    /// Clone the inner `Arc<AtomicU8>` so callers (e.g. `/readyz`) can read the
    /// current gate state without going through [`admit`](Self::admit).
    pub fn state_handle(&self) -> Arc<AtomicU8> {
        self.state.clone()
    }

    pub fn admit(&self) -> Admit {
        match self.state.load(Ordering::Acquire) {
            1 => {
                metrics::counter!("gw_backpressure_rejects_total").increment(1);
                metrics::gauge!("gw_backpressure_active").set(1.0);
                Admit::Reject
            }
            _ => {
                metrics::gauge!("gw_backpressure_active").set(0.0);
                Admit::Pass
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysCritical;
    impl HealthHook for AlwaysCritical {
        fn name(&self) -> &'static str {
            "always_crit"
        }
        fn check(&self) -> HealthSignal {
            HealthSignal::Critical
        }
    }
    struct AlwaysHealthy;
    impl HealthHook for AlwaysHealthy {
        fn name(&self) -> &'static str {
            "always_ok"
        }
        fn check(&self) -> HealthSignal {
            HealthSignal::Healthy
        }
    }

    #[test]
    fn any_critical_makes_aggregate_critical() {
        let mut m = SystemHealthMonitor::new(3);
        m.add_hook(AlwaysHealthy).add_hook(AlwaysCritical);
        assert_eq!(m.evaluate(), HealthSignal::Critical);
        let gate = BackpressureGate::from_state(m.state_handle());
        assert!(matches!(gate.admit(), Admit::Reject));
    }

    struct SwitchableHook {
        crit: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl HealthHook for SwitchableHook {
        fn name(&self) -> &'static str {
            "switch"
        }
        fn check(&self) -> HealthSignal {
            if self.crit.load(Ordering::Acquire) {
                HealthSignal::Critical
            } else {
                HealthSignal::Healthy
            }
        }
    }

    #[test]
    fn hysteresis_requires_consecutive_healthy_ticks() {
        let crit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut m = SystemHealthMonitor::new(3);
        m.add_hook(SwitchableHook { crit: crit.clone() });
        assert_eq!(m.evaluate(), HealthSignal::Critical);
        assert_eq!(m.state_handle().load(Ordering::Acquire), 1);
        crit.store(false, Ordering::Release);
        assert_eq!(m.evaluate(), HealthSignal::Healthy);
        assert_eq!(
            m.state_handle().load(Ordering::Acquire),
            1,
            "still critical after 1 healthy tick"
        );
        m.evaluate();
        assert_eq!(
            m.state_handle().load(Ordering::Acquire),
            1,
            "still critical after 2 healthy ticks"
        );
        m.evaluate();
        assert_eq!(
            m.state_handle().load(Ordering::Acquire),
            0,
            "healthy after 3 ticks"
        );
    }
}
