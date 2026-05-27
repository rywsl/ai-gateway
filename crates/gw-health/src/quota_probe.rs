//! Optional quota probe for keys in cooldown.
//!
//! When enabled, a background tokio task iterates cooldown entries in
//! [`KeyHealthStore`] at a configurable interval and sends a low-cost
//! probe request (e.g. `GET /v1/models`) to the upstream using the
//! vendor key. A 2xx response clears the cooldown early; any other
//! status is logged and the cooldown is left untouched (original TTL
//! preserved).
//!
//! Probe requests bypass the billing/ledger pipeline — they produce no
//! `usage:records`.
//!
//! Default: **disabled** (`enabled = false`). When disabled, no
//! background task is spawned and the module has zero runtime overhead.

use crate::store::KeyHealthStore;
use gw_core::ids::KeyId;
use gw_core::protocol::ApiFormat;
use std::sync::Arc;
use std::time::Duration;

/// Configuration for the quota probe background task.
#[derive(Clone, Debug)]
pub struct QuotaProbeCfg {
    pub enabled: bool,
    pub interval_secs: u64,
    pub probe_endpoint: String,
}

impl Default for QuotaProbeCfg {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: 30,
            probe_endpoint: "/v1/models".to_string(),
        }
    }
}

/// Outcome of a single probe request. Returned by [`ProbeTransport::probe`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeResult {
    /// Upstream returned 2xx — key is recovered.
    Success,
    /// Upstream returned non-2xx or the request failed.
    Failure,
}

/// Abstraction over the HTTP call so unit tests can provide a mock
/// without network I/O. Implementations live outside `gw-health` (e.g.
/// in `gw-gateway`) where `reqwest` and `EntityStore` are available.
#[async_trait::async_trait]
pub trait ProbeTransport: Send + Sync + 'static {
    /// Send a lightweight probe request for `key_id` to the upstream
    /// `probe_endpoint`. The implementation is responsible for resolving
    /// the vendor base URL and injecting the vendor API key.
    async fn probe(&self, key_id: &KeyId, fmt: ApiFormat, probe_endpoint: &str) -> ProbeResult;
}

/// Runs the quota probe loop. Call [`QuotaProbeRunner::spawn`] to start
/// the background task; it returns immediately with a `JoinHandle`.
pub struct QuotaProbeRunner {
    pub health: Arc<KeyHealthStore>,
    pub transport: Arc<dyn ProbeTransport>,
    pub cfg: QuotaProbeCfg,
}

impl QuotaProbeRunner {
    /// Spawn the background probe task. Returns `None` when disabled
    /// (zero overhead). The returned `JoinHandle` is intentionally
    /// drop-safe — dropping it does not cancel the task.
    pub fn spawn(self) -> Option<tokio::task::JoinHandle<()>> {
        if !self.cfg.enabled {
            tracing::info!(
                target: "gw_health::quota_probe",
                "quota probe disabled; background task not started"
            );
            return None;
        }

        let interval = Duration::from_secs(self.cfg.interval_secs.max(1));
        let endpoint = self.cfg.probe_endpoint.clone();
        let health = self.health;
        let transport = self.transport;

        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                probe_cycle(&health, &*transport, &endpoint).await;
            }
        });

        tracing::info!(
            target: "gw_health::quota_probe",
            interval_secs = self.cfg.interval_secs,
            endpoint = %self.cfg.probe_endpoint,
            "quota probe background task started"
        );

        Some(handle)
    }
}

/// One full probe cycle: snapshot cooldown entries, probe each, clear on
/// success.
async fn probe_cycle(health: &KeyHealthStore, transport: &dyn ProbeTransport, endpoint: &str) {
    let entries = health.snapshot_entries();
    for entry in entries {
        if !entry.health.in_cooldown {
            continue;
        }

        let result = transport.probe(&entry.key_id, entry.format, endpoint).await;

        let label = match result {
            ProbeResult::Success => "success",
            ProbeResult::Failure => "failure",
        };

        metrics::counter!(
            "gw_quota_probe_total",
            "key_id" => entry.key_id.as_ref().to_string(),
            "result" => label,
        )
        .increment(1);

        match result {
            ProbeResult::Success => {
                tracing::info!(
                    target: "gw_health::quota_probe",
                    key_id = %entry.key_id.as_ref(),
                    format = %entry.format.as_str(),
                    "probe success — clearing cooldown"
                );
                health.clear_cooldown(&entry.key_id, entry.format);
            }
            ProbeResult::Failure => {
                tracing::debug!(
                    target: "gw_health::quota_probe",
                    key_id = %entry.key_id.as_ref(),
                    format = %entry.format.as_str(),
                    "probe failed — keeping cooldown"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cooldown::{CooldownDirective, CooldownReason};
    use gw_core::protocol::ProtocolKind;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::Mutex as TokioMutex;

    /// Mock transport that returns a predetermined sequence of results.
    struct MockTransport {
        results: TokioMutex<Vec<ProbeResult>>,
        call_count: AtomicU32,
    }

    impl MockTransport {
        fn new(results: Vec<ProbeResult>) -> Self {
            Self {
                results: TokioMutex::new(results),
                call_count: AtomicU32::new(0),
            }
        }

        fn calls(&self) -> u32 {
            self.call_count.load(Ordering::Acquire)
        }
    }

    #[async_trait::async_trait]
    impl ProbeTransport for MockTransport {
        async fn probe(
            &self,
            _key_id: &KeyId,
            _fmt: ApiFormat,
            _probe_endpoint: &str,
        ) -> ProbeResult {
            self.call_count.fetch_add(1, Ordering::AcqRel);
            let mut q = self.results.lock().await;
            if q.is_empty() {
                ProbeResult::Failure
            } else {
                q.remove(0)
            }
        }
    }

    #[tokio::test]
    async fn test_disabled_no_task() {
        let health = Arc::new(KeyHealthStore::default());
        let transport = Arc::new(MockTransport::new(vec![]));
        let runner = QuotaProbeRunner {
            health,
            transport,
            cfg: QuotaProbeCfg {
                enabled: false,
                interval_secs: 1,
                probe_endpoint: "/v1/models".to_string(),
            },
        };
        let handle = runner.spawn();
        assert!(handle.is_none(), "disabled config must not spawn a task");
    }

    #[tokio::test(start_paused = true)]
    async fn test_probe_success_recovery() {
        let health = Arc::new(KeyHealthStore::default());
        let key = KeyId::from("vk-probe-ok");
        let fmt = ProtocolKind::OpenAi;

        // Put key in cooldown
        health.apply_cooldown(
            &key,
            fmt,
            CooldownDirective {
                reason: CooldownReason::Status429RateLimit,
                ttl_secs: 600,
                reset_consecutive: false,
            },
        );
        assert!(!health.is_available(&key, fmt), "key should be in cooldown");

        let transport = Arc::new(MockTransport::new(vec![ProbeResult::Success]));
        let health_clone = health.clone();

        // Run one probe cycle directly
        probe_cycle(&health_clone, &*transport, "/v1/models").await;

        assert_eq!(transport.calls(), 1, "probe must have been called");
        assert!(
            health.is_available(&key, fmt),
            "key should be available after successful probe"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_probe_failure_keeps_cooldown() {
        let health = Arc::new(KeyHealthStore::default());
        let key = KeyId::from("vk-probe-fail");
        let fmt = ProtocolKind::OpenAi;

        health.apply_cooldown(
            &key,
            fmt,
            CooldownDirective {
                reason: CooldownReason::Status429RateLimit,
                ttl_secs: 600,
                reset_consecutive: false,
            },
        );
        assert!(!health.is_available(&key, fmt));

        let transport = Arc::new(MockTransport::new(vec![ProbeResult::Failure]));
        let health_clone = health.clone();

        probe_cycle(&health_clone, &*transport, "/v1/models").await;

        assert_eq!(transport.calls(), 1);
        assert!(
            !health.is_available(&key, fmt),
            "key must stay in cooldown after failed probe"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_no_probe_for_healthy_keys() {
        let health = Arc::new(KeyHealthStore::default());
        let key = KeyId::from("vk-healthy");
        let fmt = ProtocolKind::OpenAi;

        // Touch key so it exists, but do NOT apply cooldown
        let _ = health.is_available(&key, fmt);

        let transport = Arc::new(MockTransport::new(vec![]));
        let health_clone = health.clone();

        probe_cycle(&health_clone, &*transport, "/v1/models").await;

        assert_eq!(transport.calls(), 0, "healthy keys must not be probed");
    }

    #[tokio::test(start_paused = true)]
    async fn test_spawn_interval_timing() {
        let health = Arc::new(KeyHealthStore::default());
        let key = KeyId::from("vk-timing");
        let fmt = ProtocolKind::OpenAi;

        health.apply_cooldown(
            &key,
            fmt,
            CooldownDirective {
                reason: CooldownReason::Status429RateLimit,
                ttl_secs: 600,
                reset_consecutive: false,
            },
        );

        let transport = Arc::new(MockTransport::new(vec![
            ProbeResult::Failure,
            ProbeResult::Failure,
            ProbeResult::Failure,
        ]));

        let runner = QuotaProbeRunner {
            health: health.clone(),
            transport: transport.clone(),
            cfg: QuotaProbeCfg {
                enabled: true,
                interval_secs: 5,
                probe_endpoint: "/v1/models".to_string(),
            },
        };

        let handle = runner.spawn().expect("enabled config must spawn");

        // Let the spawned task start and reach its first await (the
        // skip-tick + the blocking second tick).
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // Advance past one interval; multiple yields so the spawned task
        // processes the tick + runs probe_cycle to completion.
        tokio::time::advance(Duration::from_secs(6)).await;
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(transport.calls(), 1, "first probe after one interval");

        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(transport.calls(), 2, "second probe after two intervals");

        handle.abort();
    }
}
