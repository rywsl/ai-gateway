use crate::task_metrics;

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use gw_billing::formulas::{calculate_settle_amount, TaskBillingResult};
use gw_task::contract::ClaimedTask;
use gw_task::{TaskContract, TaskKind, TaskRecord, TaskStatus};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskTimeouts {
    pub total: Duration,
    pub submit: Duration,
}

impl Default for TaskTimeouts {
    fn default() -> Self {
        Self {
            total: Duration::from_secs(120),
            submit: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Debug, Eq)]
pub struct TaskTimeoutKey {
    vendor_id: String,
    kind: TaskKind,
}

impl TaskTimeoutKey {
    #[must_use]
    pub fn new(vendor_id: impl Into<String>, kind: TaskKind) -> Self {
        Self {
            vendor_id: vendor_id.into(),
            kind,
        }
    }
}

impl PartialEq for TaskTimeoutKey {
    fn eq(&self, other: &Self) -> bool {
        self.vendor_id == other.vendor_id && self.kind == other.kind
    }
}

impl Hash for TaskTimeoutKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.vendor_id.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Debug)]
pub struct ExecLoopConfig {
    pub consumer_name: String,
    pub idle_sleep: Duration,
    pub default_timeout: TaskTimeouts,
    pub timeouts: HashMap<TaskTimeoutKey, TaskTimeouts>,
    pub global_concurrency: usize,
    pub per_vendor_concurrency: usize,
    pub per_model_concurrency: usize,
    pub per_key_concurrency: usize,
    pub cancellation_token: CancellationToken,
}

impl Default for ExecLoopConfig {
    fn default() -> Self {
        Self {
            consumer_name: "task-worker".to_string(),
            idle_sleep: Duration::from_millis(100),
            default_timeout: TaskTimeouts::default(),
            timeouts: HashMap::new(),
            global_concurrency: 128,
            per_vendor_concurrency: 32,
            per_model_concurrency: 32,
            per_key_concurrency: 32,
            cancellation_token: CancellationToken::new(),
        }
    }
}

#[derive(Clone)]
pub struct ExecLoop<C> {
    contract: C,
    http: reqwest::Client,
    cfg: ExecLoopConfig,
    limits: Arc<ConcurrencyLimits>,
}

struct ConcurrencyLimits {
    global: Arc<Semaphore>,
    vendor: Arc<Semaphore>,
    model: Arc<Semaphore>,
    key: Arc<Semaphore>,
}

struct ConcurrencyPermit {
    _global: OwnedSemaphorePermit,
    _vendor: OwnedSemaphorePermit,
    _model: OwnedSemaphorePermit,
    _key: OwnedSemaphorePermit,
}

impl ConcurrencyLimits {
    fn new(cfg: &ExecLoopConfig) -> Self {
        Self {
            global: Arc::new(Semaphore::new(cfg.global_concurrency.max(1))),
            vendor: Arc::new(Semaphore::new(cfg.per_vendor_concurrency.max(1))),
            model: Arc::new(Semaphore::new(cfg.per_model_concurrency.max(1))),
            key: Arc::new(Semaphore::new(cfg.per_key_concurrency.max(1))),
        }
    }

    async fn acquire(&self) -> anyhow::Result<ConcurrencyPermit> {
        Ok(ConcurrencyPermit {
            _global: self
                .global
                .clone()
                .acquire_owned()
                .await
                .context("acquire global task permit")?,
            _vendor: self
                .vendor
                .clone()
                .acquire_owned()
                .await
                .context("acquire vendor task permit")?,
            _model: self
                .model
                .clone()
                .acquire_owned()
                .await
                .context("acquire model task permit")?,
            _key: self
                .key
                .clone()
                .acquire_owned()
                .await
                .context("acquire key task permit")?,
        })
    }
}

impl<C> ExecLoop<C>
where
    C: TaskContract + Clone + Send + Sync + 'static,
{
    #[must_use]
    pub fn new(contract: C, http: reqwest::Client, cfg: ExecLoopConfig) -> Self {
        let limits = Arc::new(ConcurrencyLimits::new(&cfg));
        Self {
            contract,
            http,
            cfg,
            limits,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                () = self.cfg.cancellation_token.cancelled() => return Ok(()),
                result = self.run_once() => {
                    if let Err(err) = result {
                        tracing::warn!(target: "gw_task_runtime::exec_loop", error = %err, "task exec loop iteration failed");
                    }
                }
            }
        }
    }

    pub async fn run_once(&self) -> anyhow::Result<bool> {
        let Some(claimed) = self
            .contract
            .claim(&self.cfg.consumer_name)
            .await
            .map_err(|err| anyhow!("claim task: {err}"))?
        else {
            tokio::select! {
                () = self.cfg.cancellation_token.cancelled() => {}
                () = tokio::time::sleep(self.cfg.idle_sleep) => {}
            }
            return Ok(false);
        };

        let _permit = self.limits.acquire().await?;
        metrics::counter!(task_metrics::TASK_CLAIMED_TOTAL).increment(1);
        self.execute_claimed(claimed).await?;
        Ok(true)
    }

    async fn execute_claimed(&self, claimed: ClaimedTask) -> anyhow::Result<()> {
        let timeouts = self.timeouts_for(&claimed.record);
        let task_id = claimed.record.id;
        self.contract
            .update_running(&task_id, claimed.lease_until_ms, None)
            .await
            .map_err(|err| anyhow!("update task running: {err}"))?;

        let start = std::time::Instant::now();
        let execution = self.call_upstream(&claimed.record, timeouts.submit);
        let result = tokio::time::timeout(timeouts.total, execution).await;
        let elapsed = start.elapsed();
        metrics::histogram!(task_metrics::TASK_EXECUTION_DURATION_SECONDS)
            .record(elapsed.as_secs_f64());
        match result {
            Ok(Ok(success)) => {
                if let Some(provider_task_id) = success.provider_task_id.clone() {
                    self.contract
                        .update_running(&task_id, claimed.lease_until_ms, Some(provider_task_id))
                        .await
                        .map_err(|err| anyhow!("update provider task id: {err}"))?;
                }
                self.contract
                    .settle(
                        &task_id,
                        TaskStatus::Succeeded,
                        Some(success.payload),
                        None,
                        settle_amount(&claimed.record, true, timeouts.total),
                    )
                    .await
                    .map_err(|err| anyhow!("settle succeeded task: {err}"))?;
                metrics::counter!(task_metrics::TASK_SETTLED_TOTAL, "status" => "succeeded")
                    .increment(1);
            }
            Ok(Err(err)) => {
                self.contract
                    .settle(&task_id, TaskStatus::Failed, None, Some(err.to_string()), 0)
                    .await
                    .map_err(|settle_err| {
                        anyhow!("settle failed task after {err}: {settle_err}")
                    })?;
                metrics::counter!(task_metrics::TASK_SETTLED_TOTAL, "status" => "failed")
                    .increment(1);
            }
            Err(_) => {
                self.contract
                    .settle(
                        &task_id,
                        TaskStatus::Failed,
                        None,
                        Some(format!(
                            "task execution timeout after {} ms",
                            timeouts.total.as_millis()
                        )),
                        0,
                    )
                    .await
                    .map_err(|err| anyhow!("settle timed-out task: {err}"))?;
                metrics::counter!(task_metrics::TASK_SETTLED_TOTAL, "status" => "failed")
                    .increment(1);
            }
        }
        Ok(())
    }

    fn timeouts_for(&self, record: &TaskRecord) -> TaskTimeouts {
        self.cfg
            .timeouts
            .get(&TaskTimeoutKey::new(record.vendor_id.clone(), record.kind))
            .copied()
            .unwrap_or(self.cfg.default_timeout)
    }

    async fn call_upstream(
        &self,
        record: &TaskRecord,
        submit_timeout: Duration,
    ) -> anyhow::Result<UpstreamSuccess> {
        let url = upstream_url(record)?;
        let body = upstream_body(record);
        let response = self
            .http
            .post(url)
            .timeout(submit_timeout)
            .json(&body)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    anyhow!(
                        "upstream request timeout after {} ms",
                        submit_timeout.as_millis()
                    )
                } else {
                    anyhow!(err).context("send upstream task request")
                }
            })?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("read upstream task response")?;
        if !status.is_success() {
            return Err(anyhow!("upstream status {}: {}", status.as_u16(), text));
        }
        let payload = if text.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&text).context("decode upstream task response json")?
        };
        let provider_task_id = payload
            .get("provider_task_id")
            .or_else(|| payload.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        Ok(UpstreamSuccess {
            payload,
            provider_task_id,
        })
    }
}

fn settle_amount(record: &TaskRecord, succeeded: bool, elapsed: Duration) -> i64 {
    calculate_settle_amount(
        record.kind,
        &TaskBillingResult {
            actual_duration_secs: elapsed.as_secs(),
            succeeded,
            vendor_multiplier: 1.0,
        },
    )
}

struct UpstreamSuccess {
    payload: serde_json::Value,
    provider_task_id: Option<String>,
}

fn upstream_url(record: &TaskRecord) -> anyhow::Result<String> {
    record
        .input_payload
        .get("upstream_url")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| record.vendor_key_hint.clone())
        .ok_or_else(|| anyhow!("task {} missing upstream_url/vendor_key_hint", record.id))
}

fn upstream_body(record: &TaskRecord) -> serde_json::Value {
    record
        .input_payload
        .get("body")
        .cloned()
        .unwrap_or_else(|| record.input_payload.clone())
}

#[allow(dead_code)]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
