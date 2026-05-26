use crate::task_metrics;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context};
use fred::clients::RedisPool;
use fred::interfaces::StreamsInterface;
use fred::types::{RedisValue, XReadResponse};
use gw_task::contract::CancelReason;
use gw_task::{TaskContract, TaskError, TaskId, TaskStatus};
use tokio_util::sync::CancellationToken;

const CANCEL_GROUP: &str = "tasks-cancel-fanout";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelEvent {
    pub task_id: TaskId,
    pub reason: CancelReason,
}

#[allow(async_fn_in_trait)]
pub trait CancelSource: Clone + Send + Sync + 'static {
    async fn read_cancel(
        &self,
        consumer: &str,
        block: Duration,
    ) -> anyhow::Result<Option<CancelEvent>>;
}

#[derive(Clone, Debug)]
pub struct RedisCancelSource {
    pool: RedisPool,
    cancel_stream: String,
    group: String,
}

impl RedisCancelSource {
    #[must_use]
    pub fn new(pool: RedisPool) -> Self {
        Self::new_with_prefix(pool, "")
    }

    #[must_use]
    pub fn new_with_prefix(pool: RedisPool, prefix: impl Into<String>) -> Self {
        let prefix = prefix.into();
        Self {
            pool,
            cancel_stream: format!("{prefix}tasks:cancel"),
            group: CANCEL_GROUP.to_string(),
        }
    }
}

impl CancelSource for RedisCancelSource {
    async fn read_cancel(
        &self,
        consumer: &str,
        block: Duration,
    ) -> anyhow::Result<Option<CancelEvent>> {
        let client = self.pool.next();
        let _: Result<(), _> = client
            .xgroup_create(self.cancel_stream.clone(), self.group.clone(), "0", true)
            .await;
        let raw: RedisValue = client
            .xreadgroup(
                self.group.as_str(),
                consumer,
                Some(1),
                Some(block.as_millis().try_into().unwrap_or(u64::MAX)),
                false,
                vec![self.cancel_stream.as_str()],
                vec![">"],
            )
            .await
            .context("xreadgroup task cancel")?;
        if raw.is_null() || matches!(&raw, RedisValue::Array(values) if values.is_empty()) {
            return Ok(None);
        }
        let response: XReadResponse<String, String, String, String> = raw
            .into_xread_response()
            .map_err(|err| anyhow!("decode tasks:cancel xreadgroup: {err}"))?;
        let Some((_, entries)) = response.into_iter().next() else {
            return Ok(None);
        };
        let Some((stream_id, fields)) = entries.into_iter().next() else {
            return Ok(None);
        };
        let event = decode_cancel_event(&fields)?;
        let _: Result<(), _> = client
            .xack(self.cancel_stream.clone(), self.group.clone(), stream_id)
            .await;
        Ok(Some(event))
    }
}

#[derive(Clone, Default)]
pub struct RunningTaskRegistry {
    inner: Arc<RwLock<HashMap<TaskId, RunningTaskHandle>>>,
}

#[derive(Clone, Debug)]
pub struct RunningTaskHandle {
    pub token: CancellationToken,
    pub lease_until_ms: u64,
    pub provider_task_id: Option<String>,
}

impl RunningTaskRegistry {
    pub fn register(
        &self,
        task_id: TaskId,
        lease_until_ms: u64,
        provider_task_id: Option<String>,
    ) -> CancellationToken {
        let token = CancellationToken::new();
        self.insert(task_id, token.clone(), lease_until_ms, provider_task_id);
        token
    }

    pub fn insert(
        &self,
        task_id: TaskId,
        token: CancellationToken,
        lease_until_ms: u64,
        provider_task_id: Option<String>,
    ) {
        self.inner
            .write()
            .expect("running task registry poisoned")
            .insert(
                task_id,
                RunningTaskHandle {
                    token,
                    lease_until_ms,
                    provider_task_id,
                },
            );
    }

    pub fn remove(&self, task_id: &TaskId) -> Option<RunningTaskHandle> {
        self.inner
            .write()
            .expect("running task registry poisoned")
            .remove(task_id)
    }

    #[must_use]
    pub fn get(&self, task_id: &TaskId) -> Option<RunningTaskHandle> {
        self.inner
            .read()
            .expect("running task registry poisoned")
            .get(task_id)
            .cloned()
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<(TaskId, RunningTaskHandle)> {
        self.inner
            .read()
            .expect("running task registry poisoned")
            .iter()
            .map(|(id, handle)| (*id, handle.clone()))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct CancelLoopConfig {
    pub consumer_name: String,
    pub block_timeout: Duration,
    pub cancellation_token: CancellationToken,
}

impl Default for CancelLoopConfig {
    fn default() -> Self {
        Self {
            consumer_name: "task-worker-cancel".to_string(),
            block_timeout: Duration::from_secs(5),
            cancellation_token: CancellationToken::new(),
        }
    }
}

#[derive(Clone)]
pub struct CancelLoop<C, S> {
    contract: C,
    source: S,
    registry: RunningTaskRegistry,
    cfg: CancelLoopConfig,
}

impl<C, S> CancelLoop<C, S>
where
    C: TaskContract + Clone + Send + Sync + 'static,
    S: CancelSource,
{
    #[must_use]
    pub fn new(
        contract: C,
        source: S,
        registry: RunningTaskRegistry,
        cfg: CancelLoopConfig,
    ) -> Self {
        Self {
            contract,
            source,
            registry,
            cfg,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                () = self.cfg.cancellation_token.cancelled() => return Ok(()),
                result = self.run_once() => {
                    if let Err(err) = result {
                        tracing::warn!(target: "gw_task_runtime::cancel_loop", error = %err, "task cancel loop iteration failed");
                    }
                }
            }
        }
    }

    pub async fn run_once(&self) -> anyhow::Result<bool> {
        let Some(event) = self
            .source
            .read_cancel(&self.cfg.consumer_name, self.cfg.block_timeout)
            .await?
        else {
            return Ok(false);
        };
        let Some(handle) = self.registry.remove(&event.task_id) else {
            return Ok(true);
        };
        handle.token.cancel();
        metrics::counter!(task_metrics::TASK_CANCEL_RECEIVED_TOTAL).increment(1);
        match self
            .contract
            .settle(
                &event.task_id,
                TaskStatus::Cancelled,
                None,
                Some(format!("task cancelled: {:?}", event.reason)),
                0,
            )
            .await
        {
            Ok(_) | Err(TaskError::AlreadyTerminal(_)) => Ok(true),
            Err(err) => Err(anyhow!("settle cancelled task {}: {err}", event.task_id)),
        }
    }
}

fn decode_cancel_event(fields: &HashMap<String, String>) -> anyhow::Result<CancelEvent> {
    let task_id = fields
        .get("task_id")
        .ok_or_else(|| anyhow!("tasks:cancel entry missing task_id"))?
        .parse::<TaskId>()
        .context("parse cancel task_id")?;
    let reason = fields
        .get("reason")
        .map(String::as_str)
        .map(parse_cancel_reason)
        .transpose()
        .context("parse cancel reason")?
        .unwrap_or(CancelReason::Client);
    Ok(CancelEvent { task_id, reason })
}

fn parse_cancel_reason(value: &str) -> Result<CancelReason, serde_json::Error> {
    serde_json::from_str(&format!("\"{value}\""))
}
