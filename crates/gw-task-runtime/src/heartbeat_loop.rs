use crate::task_metrics;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use gw_task::{TaskContract, TaskError};
use tokio_util::sync::CancellationToken;

use crate::cancel_loop::RunningTaskRegistry;

#[derive(Clone, Debug)]
pub struct HeartbeatLoopConfig {
    pub heartbeat_interval: Duration,
    pub lease_extension: Duration,
    pub cancellation_token: CancellationToken,
}

impl Default for HeartbeatLoopConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(15),
            lease_extension: Duration::from_secs(60),
            cancellation_token: CancellationToken::new(),
        }
    }
}

#[derive(Clone)]
pub struct HeartbeatLoop<C> {
    contract: C,
    registry: RunningTaskRegistry,
    cfg: HeartbeatLoopConfig,
}

impl<C> HeartbeatLoop<C>
where
    C: TaskContract + Clone + Send + Sync + 'static,
{
    #[must_use]
    pub fn new(contract: C, registry: RunningTaskRegistry, cfg: HeartbeatLoopConfig) -> Self {
        Self {
            contract,
            registry,
            cfg,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                () = self.cfg.cancellation_token.cancelled() => return Ok(()),
                () = tokio::time::sleep(self.cfg.heartbeat_interval) => {
                    if let Err(err) = self.run_once().await {
                        tracing::warn!(target: "gw_task_runtime::heartbeat_loop", error = %err, "task heartbeat loop iteration failed");
                    }
                }
            }
        }
    }

    pub async fn run_once(&self) -> anyhow::Result<usize> {
        let lease_until_ms = now_ms().saturating_add(self.cfg.lease_extension.as_millis() as u64);
        let mut updated = 0;
        for (task_id, handle) in self.registry.snapshot() {
            if handle.token.is_cancelled() {
                continue;
            }
            match self
                .contract
                .update_running(&task_id, lease_until_ms, handle.provider_task_id.clone())
                .await
            {
                Ok(_) => {
                    updated += 1;
                    metrics::counter!(task_metrics::TASK_HEARTBEAT_TOTAL).increment(1);
                }
                Err(TaskError::AlreadyTerminal(_)) | Err(TaskError::NotFound(_)) => {
                    self.registry.remove(&task_id);
                }
                Err(err) => return Err(anyhow!("heartbeat task lease {task_id}: {err}")),
            }
        }
        Ok(updated)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
