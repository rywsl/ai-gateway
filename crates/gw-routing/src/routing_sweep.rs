//! Periodic sweep of in-memory routing state. Supervisor-wrapped.
//!
//! Sweeps:
//! - `ModelFrequencyTracker::sweep_stale`
//! - `ReservationGlobalView::sweep_stale_instances`
//! - `LatencyTracker::evict_stale`
//! - `AffinityManager::sweep_expired`

use crate::affinity::AffinityManager;
use crate::latency::LatencyTracker;
use crate::reservation::{ModelFrequencyTracker, ReservationGlobalView};
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tracing::debug;

const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
const STALE_THRESHOLD_MS: u64 = 3_600_000; // 1h default

pub fn spawn(
    freq: Option<Arc<ModelFrequencyTracker>>,
    view: Option<Arc<ReservationGlobalView>>,
    latency: Arc<LatencyTracker>,
    affinity: Arc<AffinityManager>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    Supervisor::new("routing_sweep", RestartPolicy::default(), shutdown).spawn(move |tok| {
        let freq = freq.clone();
        let view = view.clone();
        let latency = latency.clone();
        let affinity = affinity.clone();
        async move {
            let mut tick = tokio::time::interval(SWEEP_INTERVAL);
            tick.tick().await; // skip immediate first tick
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        tracing::info!(
                            target: "gw_routing::routing_sweep",
                            "shutdown requested"
                        );
                        return Ok(());
                    }
                    _ = tick.tick() => {
                        let freq_removed = freq.as_ref().map_or(0, |f| f.sweep_stale(STALE_THRESHOLD_MS));
                        let view_removed = view.as_ref().map_or(0, |v| v.sweep_stale_instances(STALE_THRESHOLD_MS));
                        latency.evict_stale(Duration::from_millis(STALE_THRESHOLD_MS));
                        let affinity_removed = affinity.sweep_expired(Instant::now());
                        if freq_removed > 0 || view_removed > 0 || affinity_removed > 0 {
                            debug!(
                                target: "gw_routing::routing_sweep",
                                freq_removed,
                                view_removed,
                                affinity_removed,
                                "routing sweep tick"
                            );
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_runtime::ShutdownCoordinator;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_respects_shutdown_token() {
        let coord = ShutdownCoordinator::default();
        let token = coord.token();
        let handle = spawn(
            Some(Arc::new(ModelFrequencyTracker::default())),
            Some(Arc::new(ReservationGlobalView::default())),
            Arc::new(LatencyTracker::default()),
            Arc::new(AffinityManager::default()),
            token,
        );
        coord.token().cancel();
        let exit = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            exit.is_ok(),
            "routing_sweep did not exit within 5s of shutdown"
        );
    }
}
