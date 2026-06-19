//! MemoryPressureHook — container-aware memory pressure via `sysinfo`.

use super::{HealthHook, HealthSignal};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
struct MemoryUsage {
    used_bytes: u64,
    limit_bytes: u64,
}

trait MemoryUsageSampler: Send + Sync + 'static {
    fn sample(&self) -> Option<MemoryUsage>;
}

struct SystemMemorySampler {
    system: Mutex<sysinfo::System>,
}

impl SystemMemorySampler {
    fn new() -> Self {
        Self {
            system: Mutex::new(sysinfo::System::new()),
        }
    }
}

impl MemoryUsageSampler for SystemMemorySampler {
    fn sample(&self) -> Option<MemoryUsage> {
        let mut system = self.system.lock().ok()?;
        system.refresh_memory();

        if let Some(limits) = system
            .cgroup_limits()
            .filter(|limits| limits.total_memory > 0)
        {
            return Some(MemoryUsage {
                used_bytes: limits.total_memory.saturating_sub(limits.free_memory),
                limit_bytes: limits.total_memory,
            });
        }

        let total_memory = system.total_memory();
        if total_memory == 0 {
            return None;
        }

        Some(MemoryUsage {
            used_bytes: system.used_memory().min(total_memory),
            limit_bytes: total_memory,
        })
    }
}

pub struct MemoryPressureHook {
    sampler: Arc<dyn MemoryUsageSampler>,
    threshold_ratio: f64, // 0.99 = 99%
}

impl Default for MemoryPressureHook {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryPressureHook {
    pub fn new() -> Self {
        Self {
            sampler: Arc::new(SystemMemorySampler::new()),
            threshold_ratio: 0.99,
        }
    }

    #[cfg(test)]
    fn with_sampler(sampler: Arc<dyn MemoryUsageSampler>, threshold_ratio: f64) -> Self {
        Self {
            sampler,
            threshold_ratio,
        }
    }
}

impl HealthHook for MemoryPressureHook {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn check(&self) -> HealthSignal {
        let usage = match self.sampler.sample() {
            Some(v) => v,
            None => return HealthSignal::Healthy,
        };
        if usage.limit_bytes == 0 {
            return HealthSignal::Healthy;
        }
        let ratio = usage.used_bytes as f64 / usage.limit_bytes as f64;
        metrics::gauge!("gw_memory_usage_ratio").set(ratio);
        if ratio >= self.threshold_ratio {
            HealthSignal::Critical
        } else {
            HealthSignal::Healthy
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StaticMemorySampler(Option<MemoryUsage>);

    impl MemoryUsageSampler for StaticMemorySampler {
        fn sample(&self) -> Option<MemoryUsage> {
            self.0
        }
    }

    fn hook(used_bytes: u64, limit_bytes: u64) -> MemoryPressureHook {
        MemoryPressureHook::with_sampler(
            Arc::new(StaticMemorySampler(Some(MemoryUsage {
                used_bytes,
                limit_bytes,
            }))),
            0.99,
        )
    }

    #[test]
    fn under_threshold_healthy() {
        let hook = hook(100, 1000);
        assert_eq!(hook.check(), HealthSignal::Healthy);
    }

    #[test]
    fn over_threshold_critical() {
        let hook = hook(995, 1000);
        assert_eq!(hook.check(), HealthSignal::Critical);
    }

    #[test]
    fn unavailable_sample_is_healthy() {
        let hook = MemoryPressureHook::with_sampler(Arc::new(StaticMemorySampler(None)), 0.99);
        assert_eq!(hook.check(), HealthSignal::Healthy);
    }
}
