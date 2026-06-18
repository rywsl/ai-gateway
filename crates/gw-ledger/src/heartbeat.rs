//! Active pod heartbeat for Go LeaseScheduler discovery.

use fred::clients::RedisPool;
use fred::interfaces::{KeysInterface, SetsInterface};
use gw_runtime::{redis_retry, RestartPolicy, ShutdownToken, Supervisor};
use std::time::Duration;
use tokio::task::JoinHandle;

use crate::metrics::HEARTBEAT_SADD_TOTAL;

pub const ACTIVE_PODS_KEY: &str = "gw:active_pods";
pub const ACTIVE_POD_KEY_PREFIX: &str = "gw:active_pod";
pub const ACTIVE_POD_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub const ACTIVE_POD_HEARTBEAT_TTL_SECS: i64 = 15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivePodHeartbeatConfig {
    pub interval: Duration,
    pub ttl_secs: i64,
}

impl Default for ActivePodHeartbeatConfig {
    fn default() -> Self {
        Self {
            interval: ACTIVE_POD_HEARTBEAT_INTERVAL,
            ttl_secs: ACTIVE_POD_HEARTBEAT_TTL_SECS,
        }
    }
}

pub async fn renew_active_pod(
    pool: &RedisPool,
    instance_id: &str,
    ttl_secs: i64,
) -> anyhow::Result<()> {
    let pod_key = active_pod_key(instance_id);
    let _: i64 = pool.sadd(ACTIVE_PODS_KEY, instance_id).await?;
    let _: () = pool
        .set::<(), _, _>(
            &pod_key,
            "1",
            Some(fred::types::Expiration::EX(ttl_secs)),
            None,
            false,
        )
        .await?;
    let _: bool = pool.expire(ACTIVE_PODS_KEY, ttl_secs).await?;
    Ok(())
}

pub fn active_pod_key(instance_id: &str) -> String {
    format!("{ACTIVE_POD_KEY_PREFIX}:{instance_id}")
}

pub fn spawn_active_pod_heartbeat(
    pool: RedisPool,
    instance_id: String,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    spawn_active_pod_heartbeat_with_config(
        pool,
        instance_id,
        ActivePodHeartbeatConfig::default(),
        shutdown,
    )
}

pub fn spawn_active_pod_heartbeat_with_config(
    pool: RedisPool,
    instance_id: String,
    cfg: ActivePodHeartbeatConfig,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    Supervisor::new("active_pod_heartbeat", RestartPolicy::default(), shutdown).spawn(move |tok| {
        let pool = pool.clone();
        let instance_id = instance_id.clone();
        let cfg = cfg.clone();
        async move {
            run_active_pod_heartbeat(pool, instance_id, cfg, tok).await;
            Ok(())
        }
    })
}

async fn run_active_pod_heartbeat(
    pool: RedisPool,
    instance_id: String,
    cfg: ActivePodHeartbeatConfig,
    shutdown: ShutdownToken,
) {
    let mut tick = tokio::time::interval(cfg.interval);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                tracing::info!(
                    target: "gw_ledger::heartbeat",
                    instance_id = %instance_id,
                    "active pod heartbeat shutdown requested"
                );
                return;
            }
            _ = tick.tick() => {
                let retry_cfg = redis_retry::RetryConfig {
                    op_name: "active_pod_heartbeat".to_string(),
                    max_retries: 3,
                    initial_backoff: Duration::from_millis(100),
                    multipliers: vec![1.0, 2.0, 4.0],
                    jitter: 0.0,
                    total_deadline: Duration::from_secs(2),
                };
                let result = redis_retry::retry_with_shutdown(&retry_cfg, &shutdown, || {
                    let pool = pool.clone();
                    let instance_id = instance_id.clone();
                    async move { renew_active_pod(&pool, &instance_id, cfg.ttl_secs).await }
                }).await;
                match result {
                    Ok(()) => {
                        metrics::counter!(HEARTBEAT_SADD_TOTAL, "result" => "success").increment(1);
                    }
                    Err(redis_retry::RetryError::Cancelled) => {
                        tracing::info!(
                            target: "gw_ledger::heartbeat",
                            instance_id = %instance_id,
                            "active pod heartbeat retry cancelled during shutdown"
                        );
                        return;
                    }
                    Err(redis_retry::RetryError::Exhausted(err)) => {
                        metrics::counter!(HEARTBEAT_SADD_TOTAL, "result" => "failed").increment(1);
                        tracing::warn!(
                            target: "gw_ledger::heartbeat",
                            instance_id = %instance_id,
                            error = ?err,
                            "active pod heartbeat renewal failed"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fred::clients::RedisPool;
    use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};
    use gw_runtime::ShutdownCoordinator;

    #[test]
    fn default_config_matches_hard_mode_contract() {
        let cfg = ActivePodHeartbeatConfig::default();
        assert_eq!(ACTIVE_PODS_KEY, "gw:active_pods");
        assert_eq!(active_pod_key("pod-a"), "gw:active_pod:pod-a");
        assert_eq!(cfg.interval, Duration::from_secs(5));
        assert_eq!(cfg.ttl_secs, 15);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn heartbeat_task_exits_on_shutdown_before_first_successful_renewal() {
        let pool = RedisPool::new(
            RedisConfig::from_url("redis://127.0.0.1:1").expect("redis config"),
            Some(PerformanceConfig::default()),
            Some(ConnectionConfig::default()),
            Some(ReconnectPolicy::new_constant(0, 1)),
            1,
        )
        .expect("RedisPool");
        let coord = ShutdownCoordinator::default();
        let handle = spawn_active_pod_heartbeat_with_config(
            pool,
            "test-pod".to_string(),
            ActivePodHeartbeatConfig {
                interval: Duration::from_secs(60),
                ttl_secs: 15,
            },
            coord.token(),
        );

        coord.token().cancel();
        let exit = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            exit.is_ok(),
            "active pod heartbeat did not exit within 5s of shutdown"
        );
    }
}
