//! Per-tick sweep loop for in-memory ledger maps:
//! - `lease::sweep_leases` owns lease-pool cleanup hooks.
//! - `coef::sweep_orphans` evicts orphan staging versions.
//!
//! Supervised by `gw_runtime::Supervisor` (catch_unwind +
//! exponential-backoff restart). Registered with `ShutdownCoordinator`.

use crate::{coef, lease, ProtectionConfig};
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::debug;

/// Sweep cadence. 60 s matches DeductGc and is short enough that the
/// COEF_STAGING_SAFETY_MARGIN windows are honoured without unnecessary churn.
const LEDGER_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Sample the credit-lease observability gauges (D9 #17). Sets
/// `gw_credit_lease_active_users` from the LEASE_POOL protected-entry count and
/// `gw_credit_coef_table_version_active` from the active coef snapshot version.
/// Called once at spawn (immediate population) then on every sweep tick.
fn sample_credit_gauges() {
    metrics::gauge!("gw_credit_lease_active_users").set(lease::protected_user_count() as f64);
    metrics::gauge!("gw_credit_coef_table_version_active")
        .set(coef::current_snapshot().version as f64);
}

/// Spawn the periodic ledger sweep. The Supervisor recovers from panics
/// (catch_unwind + exponential-backoff restart) so a sweep iteration
/// panicking does not silently disable the loop. Returns the JoinHandle
/// so the caller can register it with the ShutdownCoordinator.
pub fn spawn(shutdown: ShutdownToken, protection_cfg: ProtectionConfig) -> JoinHandle<()> {
    Supervisor::new("ledger_sweep", RestartPolicy::default(), shutdown).spawn(
        move |tok| async move {
            // Populate the credit gauges immediately so /metrics surfaces a real
            // value at boot instead of waiting a full sweep interval.
            sample_credit_gauges();
            let mut tick = tokio::time::interval(LEDGER_SWEEP_INTERVAL);
            tick.tick().await; // skip immediate first tick (match DeductGc convention)
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        tracing::info!(target: "gw_ledger::ledger_sweep", "shutdown requested");
                        return Ok(());
                    }
                    _ = tick.tick() => {
                        sample_credit_gauges();
                        let lease_removed = lease::sweep_leases(
                            lease::now_ms(),
                            protection_cfg.lease_grace_ms,
                            protection_cfg.active_registration_ttl_secs,
                        );
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
    use crate::coef::{
        apply_coef_commit, apply_coef_entry, CoefSnapshot, Dim, COEF_CACHE, COEF_STAGING,
        TEST_COEF_LOCK,
    };
    use crate::lease::{now_ms, upsert_protected, LEASE_POOL};
    use gw_core::ids::UserId;
    use gw_runtime::ShutdownCoordinator;
    use metrics_exporter_prometheus::PrometheusBuilder;
    use std::sync::Arc;

    /// Cancellation via ShutdownToken must promptly exit the sweep task.
    /// Mirrors Crate 2's `spawn_respects_shutdown_token`. The actual
    /// ShutdownCoordinator API is `coord.token().cancel()` (no `shutdown()`
    /// method; see gw_runtime::shutdown).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_respects_shutdown_token() {
        let coord = ShutdownCoordinator::default();
        let token = coord.token();
        let handle = spawn(token, crate::ProtectionConfig::default());
        coord.token().cancel();
        let exit = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            exit.is_ok(),
            "ledger_sweep did not exit within 5s of shutdown"
        );
    }

    #[test]
    fn sample_credit_gauges_sets_active_user_count_and_coef_version() {
        let _coef_lock = TEST_COEF_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        COEF_CACHE.store(Arc::new(CoefSnapshot::empty()));
        COEF_STAGING.clear();

        let uid = UserId::from(format!("ledger-sweep-sample-{}", now_ms()));
        let before = lease::protected_user_count();
        upsert_protected(&uid, 1000, "lease-sample".into(), 77, now_ms() + 60_000);
        apply_coef_entry(77, "model-sample", Dim::Input, 1);
        assert_eq!(
            apply_coef_commit(77, 1),
            crate::coef::CommitOutcome::Activated
        );

        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, sample_credit_gauges);
        let rendered = handle.render();

        let active_users = rendered
            .lines()
            .find_map(|line| line.strip_prefix("gw_credit_lease_active_users "))
            .and_then(|value| value.parse::<f64>().ok())
            .expect("active-user gauge should render");
        assert!(
            active_users >= (before + 1) as f64,
            "expected active-user gauge to include inserted protected lease:\n{rendered}"
        );
        assert!(
            rendered.contains("gw_credit_coef_table_version_active 77"),
            "expected coef-version gauge to render active version:\n{rendered}"
        );

        LEASE_POOL.remove(&uid);
    }
}
