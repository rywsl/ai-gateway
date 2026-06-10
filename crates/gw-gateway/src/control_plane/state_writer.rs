use super::ControlPlaneObserved;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;
use gw_core::control_plane::{
    default_gateway_capability, gateway_capability_key, gateway_runtime_key, CommandConsumerState,
    GatewayRuntimeSnapshot, KeyHealthSummary, ProviderHealthSummary, CONTROL_PLANE_SCHEMA_VERSION,
};
use gw_health::KeyHealthStore;
use gw_routing::InFlightStore;
use gw_store::redis_client::RedisCtx;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct DynamicRuntimeCfg {
    interval_ms: AtomicU64,
    snapshot_ttl_ms: AtomicU64,
    command_block_ms: AtomicU64,
}

impl DynamicRuntimeCfg {
    pub fn new(interval_ms: u64, snapshot_ttl_ms: u64, command_block_ms: u64) -> Self {
        Self {
            interval_ms: AtomicU64::new(interval_ms),
            snapshot_ttl_ms: AtomicU64::new(snapshot_ttl_ms),
            command_block_ms: AtomicU64::new(command_block_ms),
        }
    }

    pub fn snapshot(&self) -> RuntimeControlValues {
        RuntimeControlValues {
            interval_ms: self.interval_ms.load(Ordering::Acquire),
            snapshot_ttl_ms: self.snapshot_ttl_ms.load(Ordering::Acquire),
            command_block_ms: self.command_block_ms.load(Ordering::Acquire),
        }
    }

    pub fn update(
        &self,
        interval_ms: u64,
        snapshot_ttl_ms: u64,
        command_block_ms: u64,
    ) -> Result<(), &'static str> {
        if interval_ms < 1_000 {
            return Err("runtime_push_interval_ms_below_min");
        }
        if snapshot_ttl_ms < interval_ms.saturating_mul(2) {
            return Err("snapshot_ttl_ms_below_two_intervals");
        }
        if !(1_000..=30_000).contains(&command_block_ms) {
            return Err("command_block_ms_out_of_range");
        }
        self.interval_ms.store(interval_ms, Ordering::Release);
        self.snapshot_ttl_ms
            .store(snapshot_ttl_ms, Ordering::Release);
        self.command_block_ms
            .store(command_block_ms, Ordering::Release);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeControlValues {
    pub interval_ms: u64,
    pub snapshot_ttl_ms: u64,
    pub command_block_ms: u64,
}

pub struct StateWriter {
    pub redis: RedisCtx,
    pub health: Arc<KeyHealthStore>,
    pub in_flight: Arc<InFlightStore>,
    pub instance_id: String,
    pub started_at_ms: i64,
    pub dynamic: Arc<DynamicRuntimeCfg>,
    pub observed: Arc<ControlPlaneObserved>,
    pub command_group: String,
    pub command_consumer: String,
}

impl StateWriter {
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                if let Err(err) = self.write_capability().await {
                    tracing::warn!(
                        target: "gw_gateway::control_plane",
                        error = %err,
                        "control-plane capability write failed"
                    );
                }
                if let Err(err) = self.write_runtime().await {
                    tracing::warn!(
                        target: "gw_gateway::control_plane",
                        error = %err,
                        "control-plane runtime snapshot write failed"
                    );
                }
                let values = self.dynamic.snapshot();
                tokio::time::sleep(Duration::from_millis(values.interval_ms.max(1_000))).await;
            }
        })
    }

    async fn write_capability(&self) -> anyhow::Result<()> {
        let payload = serde_json::to_string(&default_gateway_capability(
            self.instance_id.clone(),
            now_ms(),
        ))?;
        let client = self.redis.pool.next();
        let _: () = client
            .set(
                gateway_capability_key(&self.instance_id),
                payload,
                Some(Expiration::PX(
                    self.dynamic.snapshot().snapshot_ttl_ms.max(2_000) as i64,
                )),
                None,
                false,
            )
            .await?;
        Ok(())
    }

    async fn write_runtime(&self) -> anyhow::Result<()> {
        let values = self.dynamic.snapshot();
        let active_requests = self
            .in_flight
            .snapshot_existing()
            .into_iter()
            .map(|entry| entry.count as u64)
            .sum();
        let snapshot = GatewayRuntimeSnapshot {
            schema_version: CONTROL_PLANE_SCHEMA_VERSION,
            instance_id: self.instance_id.clone(),
            generated_at_ms: now_ms(),
            uptime_ms: now_ms().saturating_sub(self.started_at_ms) as u64,
            active_requests,
            inflight_tokens: 0,
            provider_health: Vec::<ProviderHealthSummary>::new(),
            key_health: key_health_snapshot(&self.health),
            command_consumer: CommandConsumerState {
                stream: "commands:gateway".to_string(),
                group: format!("{}:{}", self.command_group, self.instance_id),
                consumer: self.command_consumer.clone(),
                last_seen_ms: Some(
                    self.observed.last_command_stream_ms.load(Ordering::Acquire) as i64
                )
                .filter(|v| *v > 0),
            },
        };
        let payload = serde_json::to_string(&snapshot)?;
        let client = self.redis.pool.next();
        let _: () = client
            .set(
                gateway_runtime_key(&self.instance_id),
                payload,
                Some(Expiration::PX(values.snapshot_ttl_ms.max(2_000) as i64)),
                None,
                false,
            )
            .await?;
        Ok(())
    }
}

fn key_health_snapshot(health: &KeyHealthStore) -> Vec<KeyHealthSummary> {
    health
        .snapshot_entries()
        .into_iter()
        .map(|entry| KeyHealthSummary {
            key_id: entry.key_id.as_ref().to_string(),
            format: entry.format.as_str().to_string(),
            available: entry.available,
            circuit: format!("{:?}", entry.health.circuit).to_lowercase(),
            in_cooldown: entry.health.in_cooldown,
            cooldown_until_ms: entry.health.cooldown_until_ms,
            consecutive_failures: entry.health.consecutive_failures,
        })
        .collect()
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_runtime_cfg_rejects_invalid_values() {
        let cfg = DynamicRuntimeCfg::new(2_000, 10_000, 5_000);

        assert_eq!(
            cfg.update(999, 10_000, 5_000).unwrap_err(),
            "runtime_push_interval_ms_below_min"
        );
        assert_eq!(
            cfg.update(2_000, 3_999, 5_000).unwrap_err(),
            "snapshot_ttl_ms_below_two_intervals"
        );
        assert_eq!(
            cfg.update(2_000, 10_000, 999).unwrap_err(),
            "command_block_ms_out_of_range"
        );
        assert_eq!(
            cfg.snapshot(),
            RuntimeControlValues {
                interval_ms: 2_000,
                snapshot_ttl_ms: 10_000,
                command_block_ms: 5_000
            }
        );
    }
}
