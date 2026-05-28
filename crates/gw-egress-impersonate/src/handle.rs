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
/// shutdown signal. The `done_rx` watch receiver lets `shutdown` wait
/// efficiently for the supervisor task to finish rather than polling a
/// receiver count.
#[derive(Clone)]
pub struct EgressHandle {
    bind: EgressBind,
    shutdown_tx: broadcast::Sender<()>,
    /// Flipped to `true` by the supervisor task just before it returns.
    /// Used in `shutdown` to block until the supervisor has fully exited.
    done_rx: tokio::sync::watch::Receiver<bool>,
}

impl EgressHandle {
    /// Spawn the supervisor + listener. Returns a handle the gateway holds.
    ///
    /// `max_sessions` caps the number of concurrent `wreq::Client` instances
    /// in the pool (one per profile/proxy combination).
    ///
    /// This function is synchronous — all spawning is non-blocking.
    pub fn start(bind: EgressBind, max_sessions: usize) -> Self {
        let pool = Arc::new(ClientPool::new(max_sessions));
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let (done_tx, done_rx) = tokio::sync::watch::channel(false);
        let bind_clone = bind.clone();
        tokio::spawn(async move {
            supervise(bind_clone, pool, shutdown_rx).await;
            // Signal completion. If all EgressHandle clones are already dropped
            // this is a no-op (send returns Err); that's fine.
            let _ = done_tx.send(true);
        });
        Self {
            bind,
            shutdown_tx,
            done_rx,
        }
    }

    /// The bind address this handle was started with.
    pub fn bind_addr(&self) -> &EgressBind {
        &self.bind
    }

    /// Cooperative shutdown — signals the supervisor to stop accepting and
    /// waits up to `drain_timeout` for the supervisor task to finish.
    ///
    /// Uses a `watch` channel that the supervisor flips to `true` on exit,
    /// so this returns immediately once the supervisor is done rather than
    /// spinning on a poll. Falls back to the timeout if the supervisor takes
    /// too long or its task panics.
    pub async fn shutdown(self, drain_timeout: Duration) {
        // Broadcast the shutdown signal. Errors here mean no receivers (already
        // shut down), which is fine.
        let _ = self.shutdown_tx.send(());

        let mut done_rx = self.done_rx.clone();
        let _ = tokio::time::timeout(drain_timeout, async {
            // If `done_rx` already shows `true` the loop exits on the first
            // check (supervisor already exited before we called shutdown).
            while !*done_rx.borrow() {
                if done_rx.changed().await.is_err() {
                    // done_tx was dropped (supervisor task panicked); treat as done.
                    break;
                }
            }
        })
        .await;
    }
}
