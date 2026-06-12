//! RedisOomHook — rolling 5 s window of explicit Redis OOM errors.
//!
//! Callers (gw-store / gw-ledger fred wrappers) MUST invoke `record()` whenever
//! a fred error string matches "OOM command not allowed when used memory".

use super::{HealthHook, HealthSignal};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct RedisOomHook {
    inner: Arc<Mutex<VecDeque<Instant>>>,
    window: Duration,
}

impl Default for RedisOomHook {
    fn default() -> Self {
        Self::new()
    }
}

impl RedisOomHook {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            window: Duration::from_secs(5),
        }
    }
    pub fn handle(&self) -> RedisOomReporter {
        RedisOomReporter {
            inner: self.inner.clone(),
            window: self.window,
        }
    }
}

#[derive(Clone)]
pub struct RedisOomReporter {
    inner: Arc<Mutex<VecDeque<Instant>>>,
    window: Duration,
}

impl RedisOomReporter {
    pub fn record(&self) {
        let now = Instant::now();
        let mut g = self.inner.lock();
        g.push_back(now);
        // Trim while we hold the lock.
        while let Some(front) = g.front() {
            if now.duration_since(*front) > self.window {
                g.pop_front();
            } else {
                break;
            }
        }
        metrics::counter!("gw_redis_oom_errors_total").increment(1);
    }
}

impl HealthHook for RedisOomHook {
    fn name(&self) -> &'static str {
        "redis_oom"
    }
    fn check(&self) -> HealthSignal {
        let now = Instant::now();
        let mut g = self.inner.lock();
        while let Some(front) = g.front() {
            if now.duration_since(*front) > self.window {
                g.pop_front();
            } else {
                break;
            }
        }
        if g.is_empty() {
            HealthSignal::Healthy
        } else {
            HealthSignal::Critical
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_healthy() {
        let h = RedisOomHook::new();
        assert_eq!(h.check(), HealthSignal::Healthy);
    }

    #[test]
    fn record_triggers_critical_then_self_clears() {
        let h = RedisOomHook {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            window: Duration::from_millis(50),
        };
        let r = h.handle();
        r.record();
        assert_eq!(h.check(), HealthSignal::Critical);
        std::thread::sleep(Duration::from_millis(80));
        assert_eq!(h.check(), HealthSignal::Healthy);
    }
}
