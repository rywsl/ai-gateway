use crate::task_metrics;

use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use fred::clients::RedisPool;
use fred::interfaces::{KeysInterface, LuaInterface};
use fred::types::{Expiration, SetOptions};
use gw_task::scripts::RECOVER_LEASE_LUA;
use gw_task::{TaskContract, TaskError, TaskId};
use tokio_util::sync::CancellationToken;

#[allow(async_fn_in_trait)]
pub trait LeaseRecoveryStore: Clone + Send + Sync + 'static {
    async fn try_acquire_watchdog_leader(&self, ttl: Duration) -> anyhow::Result<bool>;

    async fn recover_expired(&self, now_ms: u64, limit: usize) -> anyhow::Result<Vec<TaskId>>;
}

#[derive(Clone, Debug)]
pub struct RedisLeaseRecoveryStore {
    pool: RedisPool,
    leases_key: String,
    queue_key: String,
    watchdog_lock_key: String,
    lease_seconds: u64,
    script_sha: String,
}

impl RedisLeaseRecoveryStore {
    pub async fn new(pool: RedisPool) -> anyhow::Result<Self> {
        Self::new_with_prefix(pool, "", 60).await
    }

    pub async fn new_with_prefix(
        pool: RedisPool,
        prefix: impl Into<String>,
        lease_seconds: u64,
    ) -> anyhow::Result<Self> {
        let prefix = prefix.into();
        let script_sha = pool
            .next()
            .script_load(RECOVER_LEASE_LUA)
            .await
            .context("load recover lease lua")?;
        Ok(Self {
            pool,
            leases_key: format!("{prefix}tasks:leases"),
            queue_key: format!("{prefix}tasks:queue"),
            watchdog_lock_key: format!("{prefix}lock:tasks:watchdog"),
            lease_seconds,
            script_sha,
        })
    }
}

impl LeaseRecoveryStore for RedisLeaseRecoveryStore {
    async fn try_acquire_watchdog_leader(&self, ttl: Duration) -> anyhow::Result<bool> {
        let ttl_ms = ttl.as_millis().max(1).min(i64::MAX as u128) as i64;
        let result: Option<String> = self
            .pool
            .next()
            .set(
                &self.watchdog_lock_key,
                "1",
                Some(Expiration::PX(ttl_ms)),
                Some(SetOptions::NX),
                false,
            )
            .await
            .context("acquire task watchdog leader lock")?;
        Ok(result.is_some())
    }

    async fn recover_expired(&self, now_ms: u64, limit: usize) -> anyhow::Result<Vec<TaskId>> {
        let client = self.pool.next();
        let keys = vec![self.leases_key.as_str(), self.queue_key.as_str()];
        let args = [
            now_ms.to_string(),
            limit.to_string(),
            self.lease_seconds.to_string(),
        ];
        let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let result: Vec<String> = match client
            .evalsha(&self.script_sha, keys.clone(), arg_refs.clone())
            .await
        {
            Ok(value) => value,
            Err(err) if err.to_string().contains("NOSCRIPT") => client
                .eval(RECOVER_LEASE_LUA, keys, arg_refs)
                .await
                .context("eval recover lease lua")?,
            Err(err) => return Err(anyhow!("eval recover lease lua: {err}")),
        };
        result
            .into_iter()
            .map(|id| {
                TaskId::from_str(&id).with_context(|| format!("parse recovered task id {id}"))
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct WatchdogLoopConfig {
    pub check_interval: Duration,
    pub leader_ttl: Duration,
    pub recover_limit: usize,
    pub cancellation_token: CancellationToken,
}

impl Default for WatchdogLoopConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(5),
            leader_ttl: Duration::from_secs(10),
            recover_limit: 100,
            cancellation_token: CancellationToken::new(),
        }
    }
}

#[derive(Clone)]
pub struct WatchdogLoop<C, R> {
    contract: C,
    recovery: R,
    cfg: WatchdogLoopConfig,
}

impl<C, R> WatchdogLoop<C, R>
where
    C: TaskContract + Clone + Send + Sync + 'static,
    R: LeaseRecoveryStore,
{
    #[must_use]
    pub fn new(contract: C, recovery: R, cfg: WatchdogLoopConfig) -> Self {
        Self {
            contract,
            recovery,
            cfg,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                () = self.cfg.cancellation_token.cancelled() => return Ok(()),
                () = tokio::time::sleep(self.cfg.check_interval) => {
                    if let Err(err) = self.run_once().await {
                        tracing::warn!(target: "gw_task_runtime::watchdog_loop", error = %err, "task watchdog loop iteration failed");
                    }
                }
            }
        }
    }

    pub async fn run_once(&self) -> anyhow::Result<usize> {
        if !self
            .recovery
            .try_acquire_watchdog_leader(self.cfg.leader_ttl)
            .await?
        {
            return Ok(0);
        }
        let now = now_ms();
        let expired = self
            .recovery
            .recover_expired(now, self.cfg.recover_limit)
            .await?;
        let mut recovered = 0;
        for id in expired {
            match self.contract.recover_lease(&id, now).await {
                Ok(_) => {
                    recovered += 1;
                    metrics::counter!(task_metrics::TASK_LEASE_RECOVERED_TOTAL).increment(1);
                }
                Err(TaskError::AlreadyTerminal(_)) | Err(TaskError::NotFound(_)) => {}
                Err(err) => return Err(anyhow!("recover task lease {id}: {err}")),
            }
        }
        Ok(recovered)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
