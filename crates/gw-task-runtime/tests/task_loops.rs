use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use gw_task::contract::{CancelReason, ClaimedTask, SettleOutcome};
use gw_task::{TaskContract, TaskError, TaskId, TaskKind, TaskRecord, TaskStatus};
use gw_task_runtime::cancel_loop::{
    CancelEvent, CancelLoop, CancelLoopConfig, CancelSource, RunningTaskRegistry,
};
use gw_task_runtime::heartbeat_loop::{HeartbeatLoop, HeartbeatLoopConfig};
use gw_task_runtime::watchdog_loop::{LeaseRecoveryStore, WatchdogLoop, WatchdogLoopConfig};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct MemoryContract {
    inner: Arc<Mutex<MemoryState>>,
}

#[derive(Default)]
struct MemoryState {
    records: HashMap<TaskId, TaskRecord>,
    settlements: Vec<(TaskId, TaskStatus, Option<String>, i64)>,
    running_updates: Vec<(TaskId, u64)>,
    recovered: Vec<TaskId>,
}

impl MemoryContract {
    async fn insert(&self, record: TaskRecord) {
        self.inner.lock().await.records.insert(record.id, record);
    }

    async fn settlements(&self) -> Vec<(TaskId, TaskStatus, Option<String>, i64)> {
        self.inner.lock().await.settlements.clone()
    }

    async fn running_updates(&self) -> Vec<(TaskId, u64)> {
        self.inner.lock().await.running_updates.clone()
    }

    async fn recovered(&self) -> Vec<TaskId> {
        self.inner.lock().await.recovered.clone()
    }
}

impl TaskContract for MemoryContract {
    async fn submit(&self, record: TaskRecord) -> Result<TaskId, TaskError> {
        let id = record.id;
        self.insert(record).await;
        Ok(id)
    }

    async fn load(&self, id: &TaskId) -> Result<Option<TaskRecord>, TaskError> {
        Ok(self.inner.lock().await.records.get(id).cloned())
    }

    async fn watch(&self, id: &TaskId, _wait_ms: u64) -> Result<Option<TaskRecord>, TaskError> {
        self.load(id).await
    }

    async fn request_cancel(
        &self,
        id: &TaskId,
        _reason: CancelReason,
    ) -> Result<TaskRecord, TaskError> {
        let mut inner = self.inner.lock().await;
        let record = inner.records.get_mut(id).ok_or(TaskError::NotFound(*id))?;
        record.status = TaskStatus::Cancelling;
        Ok(record.clone())
    }

    async fn claim(&self, _consumer: &str) -> Result<Option<ClaimedTask>, TaskError> {
        Ok(None)
    }

    async fn update_running(
        &self,
        id: &TaskId,
        lease_until_ms: u64,
        provider_task_id: Option<String>,
    ) -> Result<TaskRecord, TaskError> {
        let mut inner = self.inner.lock().await;
        inner.running_updates.push((*id, lease_until_ms));
        let record = inner.records.get_mut(id).ok_or(TaskError::NotFound(*id))?;
        record.status = TaskStatus::Running;
        record.lease_until_ms = Some(lease_until_ms);
        if provider_task_id.is_some() {
            record.provider_task_id = provider_task_id;
        }
        Ok(record.clone())
    }

    async fn settle(
        &self,
        id: &TaskId,
        terminal_status: TaskStatus,
        _output_payload: Option<serde_json::Value>,
        error_message: Option<String>,
        actual_micro: i64,
    ) -> Result<SettleOutcome, TaskError> {
        let mut inner = self.inner.lock().await;
        inner
            .settlements
            .push((*id, terminal_status, error_message.clone(), actual_micro));
        let record = inner.records.get_mut(id).ok_or(TaskError::NotFound(*id))?;
        record.status = terminal_status;
        record.error_message = error_message;
        record.actual_micro = Some(actual_micro);
        Ok(SettleOutcome {
            record: record.clone(),
            released_micro: record.frozen_micro,
            deducted_micro: actual_micro,
            billing_record_id: None,
        })
    }

    async fn recover_lease(&self, id: &TaskId, now_ms: u64) -> Result<TaskRecord, TaskError> {
        let mut inner = self.inner.lock().await;
        inner.recovered.push(*id);
        let record = inner.records.get_mut(id).ok_or(TaskError::NotFound(*id))?;
        record.status = TaskStatus::Queued;
        record.lease_until_ms = Some(now_ms + 60_000);
        Ok(record.clone())
    }
}

#[derive(Clone, Default)]
struct MemoryCancelSource {
    events: Arc<Mutex<VecDeque<CancelEvent>>>,
}

impl MemoryCancelSource {
    async fn push(&self, event: CancelEvent) {
        self.events.lock().await.push_back(event);
    }
}

impl CancelSource for MemoryCancelSource {
    async fn read_cancel(
        &self,
        _consumer: &str,
        _block: Duration,
    ) -> anyhow::Result<Option<CancelEvent>> {
        Ok(self.events.lock().await.pop_front())
    }
}

#[derive(Clone, Default)]
struct MemoryRecoveryStore {
    expired: Arc<Mutex<Vec<TaskId>>>,
}

impl MemoryRecoveryStore {
    async fn expire(&self, id: TaskId) {
        self.expired.lock().await.push(id);
    }
}

impl LeaseRecoveryStore for MemoryRecoveryStore {
    async fn recover_expired(&self, _now_ms: u64, _limit: usize) -> anyhow::Result<Vec<TaskId>> {
        Ok(std::mem::take(&mut *self.expired.lock().await))
    }
}

fn sample_record(status: TaskStatus) -> TaskRecord {
    let now = 1_700_000_000_000;
    TaskRecord {
        id: TaskId::new_v4(),
        schema_version: 1,
        kind: TaskKind::Generic,
        status,
        user_id: "user-1".to_string(),
        child_id: None,
        api_key_id: "api-key-1".to_string(),
        client_protocol: "openai".to_string(),
        vendor_id: "vendor-1".to_string(),
        vendor_key_hint: None,
        model: Some("model-1".to_string()),
        created_at_ms: now,
        updated_at_ms: now,
        submitted_ts_ms: now,
        expires_at_ms: now + 86_400_000,
        claimed_ts_ms: None,
        ended_ts_ms: None,
        lease_until_ms: Some(now + 60_000),
        worker_id: Some("worker-test".to_string()),
        trace_id: None,
        request_id: None,
        provider_task_id: Some("provider-1".to_string()),
        provider_first_response_ms: None,
        input_payload: serde_json::json!({}),
        output_payload: None,
        error_message: None,
        frozen_micro: 100,
        actual_micro: None,
        duration_seconds: None,
        billing_record_id: None,
    }
}

#[tokio::test]
async fn test_cancel_loop_receives_cancel() {
    let contract = MemoryContract::default();
    let source = MemoryCancelSource::default();
    let registry = RunningTaskRegistry::default();
    let task = sample_record(TaskStatus::Running);
    contract.insert(task.clone()).await;
    registry.register(
        task.id,
        task.lease_until_ms.unwrap(),
        task.provider_task_id.clone(),
    );
    source
        .push(CancelEvent {
            task_id: task.id,
            reason: CancelReason::Client,
        })
        .await;

    let loop_ = CancelLoop::new(
        contract.clone(),
        source,
        registry,
        CancelLoopConfig::default(),
    );

    assert!(loop_.run_once().await.expect("run cancel once"));
    assert_eq!(contract.settlements().await[0].1, TaskStatus::Cancelled);
}

#[tokio::test]
async fn test_cancel_loop_aborts_running() {
    let contract = MemoryContract::default();
    let source = MemoryCancelSource::default();
    let registry = RunningTaskRegistry::default();
    let task = sample_record(TaskStatus::Running);
    contract.insert(task.clone()).await;
    let token = registry.register(
        task.id,
        task.lease_until_ms.unwrap(),
        task.provider_task_id.clone(),
    );
    source
        .push(CancelEvent {
            task_id: task.id,
            reason: CancelReason::Admin,
        })
        .await;

    let loop_ = CancelLoop::new(
        contract.clone(),
        source,
        registry,
        CancelLoopConfig::default(),
    );

    assert!(loop_.run_once().await.expect("run cancel once"));
    assert!(token.is_cancelled());
    assert_eq!(contract.settlements().await[0].1, TaskStatus::Cancelled);
}

#[tokio::test]
async fn test_cancel_releases_freeze() {
    let contract = MemoryContract::default();
    let source = MemoryCancelSource::default();
    let registry = RunningTaskRegistry::default();
    let task = sample_record(TaskStatus::Running);
    contract.insert(task.clone()).await;
    registry.register(
        task.id,
        task.lease_until_ms.unwrap(),
        task.provider_task_id.clone(),
    );
    source
        .push(CancelEvent {
            task_id: task.id,
            reason: CancelReason::Client,
        })
        .await;

    let loop_ = CancelLoop::new(
        contract.clone(),
        source,
        registry,
        CancelLoopConfig::default(),
    );

    assert!(loop_.run_once().await.expect("run cancel once"));
    let settlements = contract.settlements().await;
    assert_eq!(settlements.len(), 1);
    assert_eq!(settlements[0].1, TaskStatus::Cancelled);
    assert_eq!(settlements[0].3, 0);
    assert_eq!(
        contract.load(&task.id).await.unwrap().unwrap().actual_micro,
        Some(0)
    );
}

#[tokio::test]
async fn test_watchdog_recovers_expired_lease() {
    let contract = MemoryContract::default();
    let recovery = MemoryRecoveryStore::default();
    let task = sample_record(TaskStatus::Running);
    contract.insert(task.clone()).await;
    recovery.expire(task.id).await;

    let loop_ = WatchdogLoop::new(contract.clone(), recovery, WatchdogLoopConfig::default());

    assert_eq!(loop_.run_once().await.expect("watchdog once"), 1);
    assert_eq!(contract.recovered().await, vec![task.id]);
    assert_eq!(
        contract.load(&task.id).await.unwrap().unwrap().status,
        TaskStatus::Queued
    );
}

#[tokio::test]
async fn test_heartbeat_updates_lease() {
    let contract = MemoryContract::default();
    let registry = RunningTaskRegistry::default();
    let task = sample_record(TaskStatus::Running);
    contract.insert(task.clone()).await;
    registry.register(
        task.id,
        task.lease_until_ms.unwrap(),
        task.provider_task_id.clone(),
    );
    let cfg = HeartbeatLoopConfig {
        lease_extension: Duration::from_secs(45),
        cancellation_token: CancellationToken::new(),
        ..HeartbeatLoopConfig::default()
    };
    let loop_ = HeartbeatLoop::new(contract.clone(), registry, cfg);

    assert_eq!(loop_.run_once().await.expect("heartbeat once"), 1);
    let updates = contract.running_updates().await;
    assert_eq!(updates.len(), 1);
    assert!(updates[0].1 > task.lease_until_ms.unwrap());
}
