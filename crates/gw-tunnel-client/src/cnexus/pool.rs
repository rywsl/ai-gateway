//! Connection-pool config + jittered exponential backoff.
//!
//! T3-G owns the actual reconnect loop on top of these primitives. The
//! jittered backoff is exposed as a free fn so transport code and tests can
//! reuse it without paying for an Arc<PoolConfig>.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub heartbeat_interval: Duration,
    pub max_streams_per_conn: u32,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// Fraction of base delay to randomize +/-. Bounded by [0.0, 0.5].
    pub backoff_jitter: f32,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(30),
            max_streams_per_conn: 256,
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            backoff_jitter: 0.2,
        }
    }
}

/// Compute the next backoff: `min * 2^attempt`, capped at `max`, then perturbed
/// by +/- `jitter` fraction.
pub fn jittered_backoff(attempt: u32, cfg: &PoolConfig) -> Duration {
    let base_ms = cfg.backoff_min.as_millis() as f64 * 2_f64.powi(attempt.min(30) as i32);
    let capped = base_ms.min(cfg.backoff_max.as_millis() as f64);
    let jitter = (rand::random::<f32>() - 0.5) * 2.0 * cfg.backoff_jitter;
    let final_ms = capped * (1.0 + jitter as f64);
    Duration::from_millis(final_ms.max(0.0) as u64)
}

use crate::cnexus::transport::{TransportConfig, WsTransport};
use crate::core::{StreamHandle, Transport, TransportError, TransportFuture};
use crate::target::StreamTarget;
use gw_core::tunnel_protocol::TunnelProtocol;
use std::sync::Arc;

/// Reconnecting transport — supervises a `WsTransport`, recreates it on
/// failure with `jittered_backoff`. Hot path readers use `current()` to grab
/// the latest live handle (or `None` if the supervisor is between attempts).
pub struct ReconnectingTransport {
    cfg: TransportConfig,
    pool_cfg: PoolConfig,
    inner: arc_swap::ArcSwapOption<WsTransport>,
}

impl ReconnectingTransport {
    pub async fn start(cfg: TransportConfig, pool_cfg: PoolConfig) -> Arc<Self> {
        let me = Arc::new(Self {
            cfg,
            pool_cfg,
            inner: arc_swap::ArcSwapOption::new(None),
        });
        let me_loop = me.clone();
        tokio::spawn(async move {
            let mut attempt: u32 = 0;
            loop {
                match WsTransport::connect(me_loop.cfg.clone()).await {
                    Ok(t) => {
                        me_loop.inner.store(Some(Arc::new(t)));
                        attempt = 0;
                        loop {
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                            let alive = me_loop
                                .inner
                                .load_full()
                                .map(|t| t.is_alive())
                                .unwrap_or(false);
                            if !alive {
                                break;
                            }
                        }
                        me_loop.inner.store(None);
                    }
                    Err(_) => {
                        attempt = attempt.saturating_add(1);
                        let backoff = jittered_backoff(attempt, &me_loop.pool_cfg);
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        });
        me
    }

    /// Return the currently-alive transport, if any.
    pub fn current(&self) -> Option<Arc<WsTransport>> {
        self.inner.load_full()
    }
}

impl Transport for ReconnectingTransport {
    fn open_stream<'a>(&'a self, target: StreamTarget) -> TransportFuture<'a> {
        Box::pin(async move {
            let transport = self.current().ok_or(TransportError::NotAlive)?;
            Transport::open_stream(transport.as_ref(), target)
                .await
                .map(|stream| stream as StreamHandle)
        })
    }

    fn alive(&self) -> bool {
        self.current()
            .map(|transport| transport.is_alive())
            .unwrap_or(false)
    }

    fn last_heartbeat_age(&self) -> std::time::Duration {
        self.current()
            .map(|transport| transport.last_pong_age())
            .unwrap_or(std::time::Duration::MAX)
    }

    fn tunnel_protocol(&self) -> TunnelProtocol {
        TunnelProtocol::Cnexus
    }

    fn close(&self) {
        if let Some(transport) = self.current() {
            Transport::close(transport.as_ref());
        }
        self.inner.store(None);
    }
}
