//! SpillDiskFullHook — Linux/Unix only; uses libc::statvfs. On non-Unix targets
//! the disk probe is a no-op: `statvfs()` returns `None`, so the hook always
//! reports `HealthSignal::Healthy`.

use super::{HealthHook, HealthSignal};
#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

pub struct SpillDiskFullHook {
    // Only read by the Unix `statvfs()` probe; unused on other targets.
    #[cfg_attr(not(unix), allow(dead_code))]
    path: PathBuf,
    threshold_used_ratio: f64, // 0.99 = 99%
}

impl SpillDiskFullHook {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            threshold_used_ratio: 0.99,
        }
    }
    pub fn with_threshold(path: PathBuf, ratio: f64) -> Self {
        Self {
            path,
            threshold_used_ratio: ratio,
        }
    }

    #[cfg(unix)]
    fn statvfs(&self) -> Option<(u64, u64)> {
        let c = CString::new(self.path.as_os_str().as_bytes()).ok()?;
        // SAFETY: zero-initialised struct passed to libc::statvfs.
        let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: c is a valid C string for the duration of the call.
        let rc = unsafe { libc::statvfs(c.as_ptr(), &mut buf) };
        if rc != 0 {
            return None;
        }
        #[cfg(target_os = "linux")]
        let (blocks, block_size, available) = (buf.f_blocks, buf.f_frsize, buf.f_bavail);
        #[cfg(not(target_os = "linux"))]
        let (blocks, block_size, available) =
            (buf.f_blocks as u64, buf.f_frsize, buf.f_bavail as u64);

        let total = blocks * block_size;
        let free = available * block_size;
        Some((total, free))
    }

    /// Non-Unix targets have no `statvfs`; report "no data" so the hook stays a
    /// healthy no-op.
    #[cfg(not(unix))]
    fn statvfs(&self) -> Option<(u64, u64)> {
        None
    }
}

impl HealthHook for SpillDiskFullHook {
    fn name(&self) -> &'static str {
        "spill_disk"
    }
    fn check(&self) -> HealthSignal {
        let (total, free) = match self.statvfs() {
            Some(v) => v,
            None => return HealthSignal::Healthy,
        };
        if total == 0 {
            return HealthSignal::Healthy;
        }
        let used_ratio = 1.0 - (free as f64 / total as f64);
        metrics::gauge!("gw_spill_disk_free_bytes").set(free as f64);
        if used_ratio >= self.threshold_used_ratio {
            HealthSignal::Critical
        } else {
            HealthSignal::Healthy
        }
    }
}
