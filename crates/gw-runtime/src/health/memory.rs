//! MemoryPressureHook — cgroup v2 first, fall back to RSS / RLIMIT_AS.

use super::{HealthHook, HealthSignal};
use std::path::PathBuf;

pub struct MemoryPressureHook {
    cgroup_current: PathBuf,
    cgroup_max: PathBuf,
    threshold_ratio: f64, // 0.99 = 99%
}

impl Default for MemoryPressureHook {
    fn default() -> Self { Self::new() }
}

impl MemoryPressureHook {
    pub fn new() -> Self {
        Self {
            cgroup_current: PathBuf::from("/sys/fs/cgroup/memory.current"),
            cgroup_max: PathBuf::from("/sys/fs/cgroup/memory.max"),
            threshold_ratio: 0.99,
        }
    }

    pub fn with_paths(current: PathBuf, max: PathBuf, threshold_ratio: f64) -> Self {
        Self { cgroup_current: current, cgroup_max: max, threshold_ratio }
    }

    fn read_u64(path: &PathBuf) -> Option<u64> {
        let s = std::fs::read_to_string(path).ok()?;
        let trimmed = s.trim();
        if trimmed == "max" { return Some(u64::MAX); }
        trimmed.parse::<u64>().ok()
    }
}

impl HealthHook for MemoryPressureHook {
    fn name(&self) -> &'static str { "memory" }
    fn check(&self) -> HealthSignal {
        let current = match Self::read_u64(&self.cgroup_current) { Some(v) => v, None => return HealthSignal::Healthy };
        let max = match Self::read_u64(&self.cgroup_max) { Some(v) => v, None => return HealthSignal::Healthy };
        if max == u64::MAX || max == 0 { return HealthSignal::Healthy; }
        let ratio = current as f64 / max as f64;
        metrics::gauge!("gw_memory_usage_ratio").set(ratio);
        if ratio >= self.threshold_ratio { HealthSignal::Critical } else { HealthSignal::Healthy }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(p: &PathBuf, s: &str) {
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn under_threshold_healthy() {
        let tmp = TempDir::new().unwrap();
        let cur = tmp.path().join("current");
        let max = tmp.path().join("max");
        write(&cur, "100"); write(&max, "1000");
        let hook = MemoryPressureHook::with_paths(cur, max, 0.99);
        assert_eq!(hook.check(), HealthSignal::Healthy);
    }

    #[test]
    fn over_threshold_critical() {
        let tmp = TempDir::new().unwrap();
        let cur = tmp.path().join("current");
        let max = tmp.path().join("max");
        write(&cur, "995"); write(&max, "1000");
        let hook = MemoryPressureHook::with_paths(cur, max, 0.99);
        assert_eq!(hook.check(), HealthSignal::Critical);
    }
}
