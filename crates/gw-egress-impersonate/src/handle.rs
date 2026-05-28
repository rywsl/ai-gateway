//! Public handle the gateway holds in its `ProxyHttp` impl.
//!
//! `EgressBind` describes where the loopback listener binds.
//! `EgressHandle` is the entry point: call `start` to spawn the listener
//! supervisor, and `shutdown` to stop it gracefully.

use crate::pool::ClientPool;
use crate::supervisor::supervise;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

// ── EgressBind ────────────────────────────────────────────────────────────────

/// Where the egress impersonation loopback listener binds.
#[derive(Clone, Debug)]
pub enum EgressBind {
    /// Unix domain socket path. The path is created at bind time and removed
    /// (if it already exists) before re-binding.
    Uds(PathBuf),
    /// Loopback TCP address. Non-loopback peers are rejected at accept time.
    Tcp(SocketAddr),
}

// ── EgressHandle ──────────────────────────────────────────────────────────────

/// Public handle the gateway holds. Wraps the supervisor task.
///
/// Obtain via [`EgressHandle::start`]. The handle is cheaply `Clone`; all
/// clones share the same broadcast channel and therefore all receive the
/// shutdown signal.
#[derive(Clone)]
pub struct EgressHandle {
    bind: EgressBind,
    shutdown_tx: broadcast::Sender<()>,
}

impl EgressHandle {
    /// Spawn the supervisor + listener. Returns a handle the gateway holds.
    ///
    /// `max_sessions` caps the number of concurrent `wreq::Client` instances
    /// in the pool (one per profile/proxy combination).
    pub async fn start(bind: EgressBind, max_sessions: usize) -> Self {
        let pool = Arc::new(ClientPool::new(max_sessions));
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let bind_clone = bind.clone();
        tokio::spawn(async move {
            supervise(bind_clone, pool, shutdown_rx).await;
        });
        Self { bind, shutdown_tx }
    }

    /// The bind address this handle was started with.
    pub fn bind_addr(&self) -> &EgressBind {
        &self.bind
    }

    /// Cooperative shutdown — signals the supervisor to stop accepting and
    /// waits up to `drain_timeout` for in-flight egress connections to drain.
    ///
    /// Best-effort: polls the subscriber count every 25 ms. If the supervisor
    /// does not exit within `drain_timeout`, returns anyway so the gateway
    /// process can terminate.
    pub async fn shutdown(self, drain_timeout: Duration) {
        // Broadcast the shutdown signal. Errors here mean no receivers (already
        // shut down), which is fine.
        let _ = self.shutdown_tx.send(());

        let deadline = tokio::time::Instant::now() + drain_timeout;
        loop {
            if self.shutdown_tx.receiver_count() == 0 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
