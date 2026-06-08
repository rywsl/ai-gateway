use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use gw_task::contract::{CancelReason, ClaimedTask, SettleOutcome};
use gw_task::{TaskContract, TaskError, TaskId, TaskKind, TaskRecord, TaskStatus};
use gw_task_runtime::exec_loop::{ExecLoop, ExecLoopConfig, TaskTimeoutKey, TaskTimeouts};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct MemoryContract {
    inner: Arc<Mutex<MemoryState>>,
}

#[derive(Default)]
struct MemoryState {
    queue: VecDeque<ClaimedTask>,
    record: Option<TaskRecord>,
    claimed: usize,
    running_updates: usize,
    settlements: Vec<(TaskStatus, Option<serde_json::Value>, Option<String>, i64)>,
}

impl MemoryContract {
    async fn enqueue(record: TaskRecord) -> Self {
        let contract = Self::default();
        let claimed = ClaimedTask {
            record: record.clone(),
            stream_id: "1-0".to_string(),
            lease_until_ms: record.lease_until_ms.unwrap_or(1_700_000_060_000),
        };
        let mut inner = contract.inner.lock().await;
        inner.record = Some(record);
        inner.queue.push_back(claimed);
        drop(inner);
        contract
    }

    async fn claimed_count(&self) -> usize {
        self.inner.lock().await.claimed
    }

    async fn running_update_count(&self) -> usize {
        self.inner.lock().await.running_updates
    }

    async fn loaded(&self) -> TaskRecord {
        self.inner.lock().await.record.clone().expect("record")
    }

    async fn settlements(
        &self,
    ) -> Vec<(TaskStatus, Option<serde_json::Value>, Option<String>, i64)> {
        self.inner.lock().await.settlements.clone()
    }
}

impl TaskContract for MemoryContract {
    async fn freeze_task(
        &self,
        _user_id: &str,
        _task_id: &TaskId,
        _amount: i64,
        _available_balance_micro: Option<i64>,
    ) -> Result<(), TaskError> {
        Ok(())
    }

    async fn release_task_freeze(
        &self,
        _user_id: &str,
        _task_id: &TaskId,
        _amount: i64,
    ) -> Result<(), TaskError> {
        Ok(())
    }

    async fn submit(&self, record: TaskRecord) -> Result<TaskId, TaskError> {
        let id = record.id;
        self.inner.lock().await.record = Some(record);
        Ok(id)
    }

    async fn load(&self, id: &TaskId) -> Result<Option<TaskRecord>, TaskError> {
        Ok(self
            .inner
            .lock()
            .await
            .record
            .clone()
            .filter(|record| record.id == *id))
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
        let record = inner
            .record
            .as_mut()
            .filter(|record| record.id == *id)
            .ok_or(TaskError::NotFound(*id))?;
        record.status = TaskStatus::Cancelling;
        Ok(record.clone())
    }

    async fn claim(&self, consumer: &str) -> Result<Option<ClaimedTask>, TaskError> {
        let mut inner = self.inner.lock().await;
        let Some(mut claimed) = inner.queue.pop_front() else {
            return Ok(None);
        };
        inner.claimed += 1;
        claimed.record.status = TaskStatus::Running;
        claimed.record.worker_id = Some(consumer.to_string());
        inner.record = Some(claimed.record.clone());
        Ok(Some(claimed))
    }

    async fn update_running(
        &self,
        id: &TaskId,
        lease_until_ms: u64,
        provider_task_id: Option<String>,
    ) -> Result<TaskRecord, TaskError> {
        let mut inner = self.inner.lock().await;
        inner.running_updates += 1;
        let record = inner
            .record
            .as_mut()
            .filter(|record| record.id == *id)
            .ok_or(TaskError::NotFound(*id))?;
        record.status = TaskStatus::Running;
        record.lease_until_ms = Some(lease_until_ms);
        record.provider_task_id = provider_task_id;
        Ok(record.clone())
    }

    async fn settle(
        &self,
        id: &TaskId,
        terminal_status: TaskStatus,
        output_payload: Option<serde_json::Value>,
        error_message: Option<String>,
        actual_micro: i64,
    ) -> Result<SettleOutcome, TaskError> {
        let mut inner = self.inner.lock().await;
        inner.settlements.push((
            terminal_status,
            output_payload.clone(),
            error_message.clone(),
            actual_micro,
        ));
        let record = inner
            .record
            .as_mut()
            .filter(|record| record.id == *id)
            .ok_or(TaskError::NotFound(*id))?;
        record.status = terminal_status;
        record.output_payload = output_payload;
        record.error_message = error_message;
        record.actual_micro = Some(actual_micro);
        Ok(SettleOutcome {
            record: record.clone(),
            released_micro: record.frozen_micro,
            deducted_micro: actual_micro,
            billing_record_id: None,
        })
    }

    async fn recover_lease(&self, id: &TaskId, _now_ms: u64) -> Result<TaskRecord, TaskError> {
        self.load(id).await?.ok_or(TaskError::NotFound(*id))
    }
}

#[tokio::test]
async fn test_settle_releases_freeze() {
    let addr = http_server(
        200,
        r#"{"provider_task_id":"provider-settle","result":"done"}"#,
        Duration::ZERO,
    )
    .await;
    let contract = MemoryContract::enqueue(sample_record(format!("http://{addr}/submit"))).await;
    let loop_ = ExecLoop::new(contract.clone(), reqwest::Client::new(), config());

    loop_.run_once().await.expect("run once");

    let settlements = contract.settlements().await;
    assert_eq!(settlements.len(), 1);
    assert_eq!(settlements[0].0, TaskStatus::Succeeded);
    assert_eq!(settlements[0].3, 250);
    assert_eq!(contract.loaded().await.actual_micro, Some(250));
}

fn sample_record(upstream_url: String) -> TaskRecord {
    let now = 1_700_000_000_000;
    TaskRecord {
        id: TaskId::new_v4(),
        schema_version: 1,
        kind: TaskKind::Generic,
        status: TaskStatus::Queued,
        user_id: "user-1".to_string(),
        child_id: None,
        api_key_id: "api-key-1".to_string(),
        client_protocol: "openai".to_string(),
        vendor_id: "vendor-1".to_string(),
        vendor_key_hint: Some("vendor-key-1".to_string()),
        model: Some("model-1".to_string()),
        created_at_ms: now,
        updated_at_ms: now,
        submitted_ts_ms: now,
        expires_at_ms: now + 86_400_000,
        claimed_ts_ms: None,
        ended_ts_ms: None,
        lease_until_ms: Some(now + 60_000),
        worker_id: None,
        trace_id: Some("trace-1".to_string()),
        request_id: Some("req-1".to_string()),
        provider_task_id: None,
        provider_first_response_ms: None,
        input_payload: serde_json::json!({"upstream_url": upstream_url, "body": {"prompt":"hello"}}),
        output_payload: None,
        error_message: None,
        frozen_micro: 100,
        actual_micro: None,
        duration_seconds: None,
        billing_record_id: None,
    }
}

async fn http_server(status: u16, body: &'static str, delay: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = [0_u8; 4096];
        let _ = socket.read(&mut buf).await;
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let response = format!(
            "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    addr
}

async fn polling_http_server(responses: Vec<(u16, &'static str)>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind polling test server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = [0_u8; 4096];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    addr
}

fn config() -> ExecLoopConfig {
    ExecLoopConfig {
        consumer_name: "worker-test".to_string(),
        idle_sleep: Duration::from_millis(5),
        default_timeout: TaskTimeouts {
            total: Duration::from_secs(2),
            submit: Duration::from_secs(2),
        },
        global_concurrency: 4,
        per_vendor_concurrency: 4,
        per_model_concurrency: 4,
        per_key_concurrency: 4,
        ..ExecLoopConfig::default()
    }
}

#[tokio::test]
async fn test_exec_loop_claims_and_executes() {
    let addr = http_server(
        200,
        r#"{"provider_task_id":"provider-1","ok":true}"#,
        Duration::ZERO,
    )
    .await;
    let contract = MemoryContract::enqueue(sample_record(format!("http://{addr}/submit"))).await;
    let loop_ = ExecLoop::new(contract.clone(), reqwest::Client::new(), config());

    loop_.run_once().await.expect("run once");

    assert_eq!(contract.claimed_count().await, 1);
    assert_eq!(
        contract.loaded().await.provider_task_id.as_deref(),
        Some("provider-1")
    );
}

#[tokio::test]
async fn test_exec_loop_updates_running_state() {
    let addr = http_server(
        200,
        r#"{"provider_task_id":"provider-2","ok":true}"#,
        Duration::ZERO,
    )
    .await;
    let contract = MemoryContract::enqueue(sample_record(format!("http://{addr}/submit"))).await;
    let loop_ = ExecLoop::new(contract.clone(), reqwest::Client::new(), config());

    loop_.run_once().await.expect("run once");

    assert!(contract.running_update_count().await >= 1);
    assert_eq!(contract.loaded().await.status, TaskStatus::Succeeded);
}

#[tokio::test]
async fn test_exec_loop_settles_on_success() {
    let addr = http_server(
        200,
        r#"{"provider_task_id":"provider-3","result":"done"}"#,
        Duration::ZERO,
    )
    .await;
    let contract = MemoryContract::enqueue(sample_record(format!("http://{addr}/submit"))).await;
    let loop_ = ExecLoop::new(contract.clone(), reqwest::Client::new(), config());

    loop_.run_once().await.expect("run once");

    let settlements = contract.settlements().await;
    assert_eq!(settlements.len(), 1);
    assert_eq!(settlements[0].0, TaskStatus::Succeeded);
    assert_eq!(
        settlements[0]
            .1
            .as_ref()
            .and_then(|value| value.get("result"))
            .and_then(|value| value.as_str()),
        Some("done")
    );
}

#[tokio::test]
async fn test_exec_loop_polls_provider_task_until_terminal() {
    let addr = polling_http_server(vec![
        (
            202,
            r#"{"provider_task_id":"provider-poll","status":"running","status_url":"/poll/provider-poll"}"#,
        ),
        (
            200,
            r#"{"provider_task_id":"provider-poll","status":"succeeded","result":"done"}"#,
        ),
    ])
    .await;
    let contract = MemoryContract::enqueue(sample_record(format!("http://{addr}/submit"))).await;
    let mut cfg = config();
    cfg.poll_initial_backoff = Duration::from_millis(1);
    cfg.poll_max_backoff = Duration::from_millis(1);
    let loop_ = ExecLoop::new(contract.clone(), reqwest::Client::new(), cfg);

    loop_.run_once().await.expect("run once");

    let settlements = contract.settlements().await;
    assert_eq!(settlements.len(), 1);
    assert_eq!(settlements[0].0, TaskStatus::Succeeded);
    assert_eq!(
        settlements[0]
            .1
            .as_ref()
            .and_then(|value| value.get("result"))
            .and_then(|value| value.as_str()),
        Some("done")
    );
    assert_eq!(
        contract.loaded().await.provider_task_id.as_deref(),
        Some("provider-poll")
    );
}

#[tokio::test]
async fn test_exec_loop_settles_on_failure() {
    let addr = http_server(500, r#"{"error":"boom"}"#, Duration::ZERO).await;
    let contract = MemoryContract::enqueue(sample_record(format!("http://{addr}/submit"))).await;
    let loop_ = ExecLoop::new(contract.clone(), reqwest::Client::new(), config());

    loop_.run_once().await.expect("run once");

    let settlements = contract.settlements().await;
    assert_eq!(settlements.len(), 1);
    assert_eq!(settlements[0].0, TaskStatus::Failed);
    assert!(settlements[0]
        .2
        .as_deref()
        .unwrap_or_default()
        .contains("upstream status 500"));
}

#[tokio::test]
async fn test_exec_loop_respects_timeout() {
    let addr = http_server(
        200,
        r#"{"provider_task_id":"late"}"#,
        Duration::from_millis(150),
    )
    .await;
    let contract = MemoryContract::enqueue(sample_record(format!("http://{addr}/submit"))).await;
    let mut cfg = config();
    cfg.default_timeout = TaskTimeouts {
        total: Duration::from_millis(30),
        submit: Duration::from_millis(30),
    };
    cfg.timeouts.insert(
        TaskTimeoutKey::new("vendor-1", TaskKind::Generic),
        cfg.default_timeout,
    );
    let loop_ = ExecLoop::new(contract.clone(), reqwest::Client::new(), cfg);

    loop_.run_once().await.expect("run once");

    let settlements = contract.settlements().await;
    assert_eq!(settlements.len(), 1);
    assert_eq!(settlements[0].0, TaskStatus::Failed);
    assert!(settlements[0]
        .2
        .as_deref()
        .unwrap_or_default()
        .contains("timeout"));
}
