//! Outbound target host:port whitelist.
//!
//! Default deny. Whitelist is process-global and managed by the bootstrap
//! layer; tests use `ResetGuard` to snapshot/restore.

use arc_swap::ArcSwap;
use std::sync::Arc;
use url::Url;

static WHITELIST: once_cell::sync::Lazy<ArcSwap<Vec<(String, u16)>>> =
    once_cell::sync::Lazy::new(|| ArcSwap::from_pointee(Vec::new()));

pub fn set_whitelist(list: Vec<(String, u16)>) {
    WHITELIST.store(Arc::new(list));
}

#[derive(Debug, thiserror::Error)]
#[error("target {0} not in whitelist")]
pub struct TargetDenied(pub String);

pub fn assert_target_allowed(url: &Url) -> Result<(), TargetDenied> {
    let host = url.host_str().unwrap_or("");
    let port = url.port_or_known_default().unwrap_or(0);
    let wl = WHITELIST.load_full();
    if wl.iter().any(|(h, p)| h == host && *p == port) {
        Ok(())
    } else {
        metrics::counter!("gw_target_filter_denied_total", "host" => host.to_string()).increment(1);
        Err(TargetDenied(format!("{host}:{port}")))
    }
}

/// RAII guard for tests: serializes parallel test sections AND snapshots /
/// restores the whitelist. Hold one for the duration of any test that calls
/// `set_whitelist`.
pub struct ResetGuard {
    snapshot: Arc<Vec<(String, u16)>>,
    _lock: parking_lot::MutexGuard<'static, ()>,
}

impl ResetGuard {
    pub fn new() -> Self {
        let lock = WHITELIST_TEST_LOCK.lock();
        Self {
            snapshot: WHITELIST.load_full(),
            _lock: lock,
        }
    }
}

impl Default for ResetGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ResetGuard {
    fn drop(&mut self) {
        WHITELIST.store(self.snapshot.clone());
    }
}

static WHITELIST_TEST_LOCK: once_cell::sync::Lazy<parking_lot::Mutex<()>> =
    once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(()));
