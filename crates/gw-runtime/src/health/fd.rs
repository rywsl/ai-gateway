//! FdPressureHook — Linux only; reads /proc/self/limits and counts /proc/self/fd.

use super::{HealthHook, HealthSignal};

pub struct FdPressureHook {
    fd_dir: std::path::PathBuf,
    limit_path: std::path::PathBuf,
    threshold_ratio: f64,
}

impl Default for FdPressureHook {
    fn default() -> Self { Self::new() }
}

impl FdPressureHook {
    pub fn new() -> Self {
        Self {
            fd_dir: std::path::PathBuf::from("/proc/self/fd"),
            limit_path: std::path::PathBuf::from("/proc/self/limits"),
            threshold_ratio: 0.99,
        }
    }

    pub fn with_paths(fd_dir: std::path::PathBuf, limit_path: std::path::PathBuf, threshold_ratio: f64) -> Self {
        Self { fd_dir, limit_path, threshold_ratio }
    }

    fn read_limit(&self) -> Option<u64> {
        let s = std::fs::read_to_string(&self.limit_path).ok()?;
        // Match a line like: "Max open files            1024                 1024                 files"
        for line in s.lines() {
            if line.starts_with("Max open files") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                // [..., "Max", "open", "files", soft, hard, unit]; the soft limit is what
                // setrlimit enforces. Pick the soft limit (parts[3]).
                if let Some(soft) = parts.get(3) {
                    if *soft == "unlimited" { return Some(u64::MAX); }
                    return soft.parse::<u64>().ok();
                }
            }
        }
        None
    }

    fn count_open(&self) -> Option<u64> {
        let mut n: u64 = 0;
        let rd = std::fs::read_dir(&self.fd_dir).ok()?;
        for ent in rd { if ent.is_ok() { n += 1; } }
        Some(n)
    }
}

impl HealthHook for FdPressureHook {
    fn name(&self) -> &'static str { "fd" }
    fn check(&self) -> HealthSignal {
        let open = match self.count_open() { Some(v) => v, None => return HealthSignal::Healthy };
        let limit = match self.read_limit() { Some(v) if v != u64::MAX => v, _ => return HealthSignal::Healthy };
        if limit == 0 { return HealthSignal::Healthy; }
        let ratio = open as f64 / limit as f64;
        metrics::gauge!("gw_fd_usage_ratio").set(ratio);
        if ratio >= self.threshold_ratio { HealthSignal::Critical } else { HealthSignal::Healthy }
    }
}
