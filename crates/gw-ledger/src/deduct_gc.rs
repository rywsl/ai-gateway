//! Periodic GC for zero-state entries in the in-memory DEDUCT DashMap.
//!
//! Prevents unbounded memory growth by scanning the map on a fixed interval
//! and removing entries where all three counters (`base_micro`,
//! `deducted_micro`, `frozen_micro`) are zero. Best-effort: uses
//! `Ordering::Relaxed` since a missed entry will be caught on the next round.

use gw_store::deduction::DEDUCT;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::debug;

/// Hardcoded 60s; can be made configurable via GatewayConfig.
const GC_INTERVAL: Duration = Duration::from_secs(60);

/// Handle to the background DEDUCT GC task.
pub struct DeductGc;

impl DeductGc {
    /// Spawn the periodic DEDUCT + USER_CIRCUIT sweep supervised by
    /// `gw_runtime::Supervisor`. The Supervisor provides catch_unwind +
    /// exponential-backoff restart so a panic in `sweep()` does not cause
    /// the in-memory DEDUCT map to grow unbounded thereafter.
    ///
    /// Returns the `JoinHandle` so the caller can register it with the
    /// `ShutdownCoordinator`. The task runs until cancelled via
    /// `ShutdownToken` or until the restart budget is exhausted.
    pub fn spawn(shutdown: gw_runtime::ShutdownToken) -> tokio::task::JoinHandle<()> {
        gw_runtime::Supervisor::new(
            "deduct_gc",
            gw_runtime::RestartPolicy::default(),
            shutdown,
        )
        .spawn(move |tok| async move {
            let mut tick = tokio::time::interval(GC_INTERVAL);
            // Skip the immediate first tick — original `tokio::time::sleep`
            // pattern waited a full interval before the first sweep; match
            // that.
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        tracing::info!(target: "gw_ledger::deduct_gc", "shutdown requested");
                        return Ok(());
                    }
                    _ = tick.tick() => {
                        let dedup_removed = Self::sweep();
                        let circuit_removed = gw_store::deduction::sweep_user_circuit();
                        if dedup_removed > 0 || circuit_removed > 0 {
                            debug!(dedup_removed, circuit_removed, "deduct GC + user-circuit sweep");
                        }
                    }
                }
            }
        })
    }

    /// Single synchronous sweep of DEDUCT. Returns the number of entries
    /// removed. Kept as a separate function so tests can exercise the
    /// sweep without spawning a Supervisor.
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

    /// New API: `DeductGc::spawn(token)` returns a JoinHandle whose task
    /// exits when the shutdown token is cancelled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_respects_shutdown_token() {
        use gw_runtime::ShutdownCoordinator;
        use std::time::Duration;

        let coord = ShutdownCoordinator::default();
        let token = coord.token();
        let handle = DeductGc::spawn(token);

        // Cancel immediately; the task should exit promptly.
        coord.token().cancel();

        // The Supervisor wraps the loop; on cancellation it should return
        // within a short window. 5s is generous.
        let exit = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            exit.is_ok(),
            "DeductGc task did not exit within 5s of shutdown"
        );
    }
}
