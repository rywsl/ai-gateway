//! Per-tick sweep loop for in-memory ledger maps:
//! - `lease::sweep_leases` owns lease-pool cleanup hooks.
//! - `coef::sweep_orphans` evicts orphan staging versions.
//!
//! Supervised by `gw_runtime::Supervisor` (catch_unwind +
//! exponential-backoff restart). Registered with `ShutdownCoordinator`.

use crate::{coef, lease};
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::debug;

/// Sweep cadence. 60 s matches DeductGc and is short enough that the
/// COEF_STAGING_SAFETY_MARGIN windows are honoured without unnecessary churn.
const LEDGER_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Spawn the periodic ledger sweep. The Supervisor recovers from panics
/// (catch_unwind + exponential-backoff restart) so a sweep iteration
/// panicking does not silently disable the loop. Returns the JoinHandle
/// so the caller can register it with the ShutdownCoordinator.
pub fn spawn(shutdown: ShutdownToken) -> JoinHandle<()> {
    Supervisor::new("ledger_sweep", RestartPolicy::default(), shutdown).spawn(
        move |tok| async move {
            let mut tick = tokio::time::interval(LEDGER_SWEEP_INTERVAL);
            tick.tick().await; // skip immediate first tick (match DeductGc convention)
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        tracing::info!(target: "gw_ledger::ledger_sweep", "shutdown requested");
                        return Ok(());
                    }
                    _ = tick.tick() => {
                        let lease_removed = lease::sweep_leases();
                        let coef_removed = coef::sweep_orphans();
                        if lease_removed > 0 || coef_removed > 0 {
                            debug!(
                                target: "gw_ledger::ledger_sweep",
                                lease_removed,
                                coef_removed,
                                "ledger sweep tick"
                            );
                        }
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_runtime::ShutdownCoordinator;

    /// Cancellation via ShutdownToken must promptly exit the sweep task.
    /// Mirrors Crate 2's `spawn_respects_shutdown_token`. The actual
    /// ShutdownCoordinator API is `coord.token().cancel()` (no `shutdown()`
    /// method; see gw_runtime::shutdown).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_respects_shutdown_token() {
        let coord = ShutdownCoordinator::default();
        let token = coord.token();
        let handle = spawn(token);
        coord.token().cancel();
        let exit = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            exit.is_ok(),
            "ledger_sweep did not exit within 5s of shutdown"
        );
    }
}
