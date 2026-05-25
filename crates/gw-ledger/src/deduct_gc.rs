//! Periodic GC for zero-state entries in the in-memory DEDUCT DashMap.
//!
//! Prevents unbounded memory growth by scanning the map on a fixed interval
//! and removing entries where all three counters (`base_micro`,
//! `deducted_micro`, `frozen_micro`) are zero. Best-effort: uses
//! `Ordering::Relaxed` since a missed entry will be caught on the next round.

use gw_store::deduction::DEDUCT;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::debug;

/// Hardcoded 60s; can be made configurable via GatewayConfig.
const GC_INTERVAL: Duration = Duration::from_secs(60);

/// Handle to the background DEDUCT GC task.
pub struct DeductGc;

impl DeductGc {
    /// Spawn a background tokio task that periodically removes zero-state
    /// entries from [`DEDUCT`]. Returns the `JoinHandle` so the caller can
    /// abort on shutdown if desired.
    pub fn start(interval: Duration) -> JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let removed = Self::sweep();
                if removed > 0 {
                    debug!(removed, "deduct GC sweep");
                }
            }
        })
    }

    /// Convenience wrapper that uses the default 60 s interval.
    pub fn start_default() -> JoinHandle<()> {
        Self::start(GC_INTERVAL)
    }

    /// Single synchronous sweep. Returns the number of entries removed.
    pub fn sweep() -> u64 {
        let mut removed: u64 = 0;
        DEDUCT.retain(|_uid, state| {
            let base = state.base_micro.load(Ordering::Relaxed);
            let deducted = state.deducted_micro.load(Ordering::Relaxed);
            let frozen = state.frozen_micro.load(Ordering::Relaxed);
            if base == 0 && deducted == 0 && frozen == 0 {
                removed += 1;
                false
            } else {
                true
            }
        });
        if removed > 0 {
            metrics::counter!("gw_deduct_gc_removed_total").increment(removed);
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::ids::UserId;
    use gw_store::deduction::DeductionState;
    use std::sync::atomic::AtomicI64;
    use std::sync::{Arc, Mutex};

    // sweep() touches all keys in the global DEDUCT, so tests that assert
    // counts must not race each other.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn zero_state() -> Arc<DeductionState> {
        Arc::new(DeductionState {
            base_micro: AtomicI64::new(0),
            deducted_micro: AtomicI64::new(0),
            frozen_micro: AtomicI64::new(0),
        })
    }

    fn nonzero_state(base: i64) -> Arc<DeductionState> {
        Arc::new(DeductionState {
            base_micro: AtomicI64::new(base),
            deducted_micro: AtomicI64::new(0),
            frozen_micro: AtomicI64::new(0),
        })
    }

    fn count_prefix(prefix: &str) -> usize {
        DEDUCT
            .iter()
            .filter(|e| e.key().as_ref().starts_with(prefix))
            .count()
    }

    /// Insert 1000 zero-state + 10 non-zero → sweep → only non-zero survive.
    #[test]
    fn gc_removes_zero_state_entries() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "gc1_";
        for i in 0..1000 {
            DEDUCT.insert(UserId::from(format!("{prefix}zero-{i}")), zero_state());
        }
        for i in 0..10 {
            DEDUCT.insert(
                UserId::from(format!("{prefix}live-{i}")),
                nonzero_state(500),
            );
        }
        assert_eq!(count_prefix(prefix), 1010);

        DeductGc::sweep();

        assert_eq!(count_prefix(prefix), 10);
        for i in 0..10 {
            assert!(DEDUCT.contains_key(&UserId::from(format!("{prefix}live-{i}"))));
        }

        for i in 0..10 {
            DEDUCT.remove(&UserId::from(format!("{prefix}live-{i}")));
        }
    }

    /// Empty prefix namespace → sweep → no panic, prefix count stays 0.
    #[test]
    fn gc_on_empty_map() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "gc2_";
        assert_eq!(count_prefix(prefix), 0);

        DeductGc::sweep();

        assert_eq!(count_prefix(prefix), 0);
    }

    /// All-zero entry removed; re-insert same key → still works.
    #[test]
    fn gc_reinsert_after_removal() {
        let _lock = SERIAL.lock().unwrap();
        let uid = UserId::from("gc3_ephemeral");
        DEDUCT.insert(uid.clone(), zero_state());
        assert!(DEDUCT.contains_key(&uid));

        DeductGc::sweep();
        assert!(!DEDUCT.contains_key(&uid));

        DEDUCT.insert(uid.clone(), nonzero_state(100));
        DeductGc::sweep();
        assert!(DEDUCT.contains_key(&uid));

        DEDUCT.remove(&uid);
    }
}
