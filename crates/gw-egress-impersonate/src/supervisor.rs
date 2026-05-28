//! Supervisor — wraps `Listener::spawn` with bounded restart on panic/error.
//!
//! Caps at 5 restarts per 60 s. Beyond that, logs critical and exits the
//! supervisor task. The process keeps running; egress is down until the next
//! process restart (gw-proxy treats connect_refused as retryable).

use crate::handle::EgressBind;
use crate::pool::ClientPool;
use crate::server::{Handler, Listener};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Run the egress listener under supervision.
///
/// Loops until either:
/// - A graceful shutdown signal is received on `shutdown_rx`.
/// - The restart rate exceeds 5 restarts / 60 s (gives up; logs critical).
///
/// On each iteration the listener is re-bound and re-spawned. The `shutdown_rx`
/// is also checked before each bind attempt so we don't restart after shutdown.
pub async fn supervise(
    bind: EgressBind,
    pool: Arc<ClientPool>,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let mut restarts: Vec<Instant> = Vec::new();

    loop {
        // Bail immediately if a shutdown was requested.
        if shutdown_rx.try_recv().is_ok() {
            break;
        }

        // Build a per-restart Handler. This is lightweight (wraps the shared
        // ClientPool Arc) and gives each restart a clean hyper connection state.
        let handler = Arc::new(Handler::new_with_pool(Arc::clone(&pool)));

        let listener = match Listener::bind(bind.clone(), handler).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("gw-egress-impersonate: listener bind failed: {e}");
                metrics::counter!(
                    "gw_egress_supervisor_restart_total",
                    "reason" => "bind_failed"
                )
                .increment(1);
                tokio::time::sleep(backoff(restarts.len())).await;
                continue;
            }
        };

        let mut handle = listener.spawn();

        // Race: listener task end vs. external shutdown signal.
        tokio::select! {
            res = &mut handle.task => {
                match res {
                    // Graceful listener exit (shutdown_tx fired inside Listener).
                    Ok(Ok(())) => break,
                    Ok(Err(e)) => tracing::warn!("gw-egress-impersonate: listener exited with error: {e}"),
                    Err(e) if e.is_panic() => {
                        tracing::error!("gw-egress-impersonate: listener panicked: {e:?}");
                    }
                    Err(e) => tracing::warn!("gw-egress-impersonate: listener join error: {e}"),
                }
            }
            _ = shutdown_rx.recv() => {
                // Forward shutdown into the listener and await clean exit.
                handle.shutdown().await;
                break;
            }
        }

        // Rate-limit restarts.
        let now = Instant::now();
        restarts.retain(|t| now.duration_since(*t) <= Duration::from_secs(60));
        restarts.push(now);

        if restarts.len() > 5 {
            tracing::error!(
                "gw-egress-impersonate: supervisor: >5 restarts/min, giving up"
            );
            metrics::counter!(
                "gw_egress_supervisor_restart_total",
                "reason" => "exhausted"
            )
            .increment(1);
            break;
        }

        metrics::counter!(
            "gw_egress_supervisor_restart_total",
            "reason" => "panic_or_err"
        )
        .increment(1);
        tokio::time::sleep(backoff(restarts.len())).await;
    }
}

fn backoff(restart_count: usize) -> Duration {
    let secs = match restart_count {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 8,
        _ => 16,
    };
    Duration::from_secs(secs)
}
