use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use gw_billing::TaskBillingConfig;
use gw_codec::AnyCodec;
use gw_core::ids::UserId;
use gw_store::deduction::{DeductionState, DEDUCT};
use gw_task::contract::{CancelReason, ClaimedTask, SettleOutcome};
use gw_task::{TaskContract, TaskError, TaskId, TaskKind, TaskRecord, TaskStatus};

fn task_record(id: TaskId, status: TaskStatus) -> TaskRecord {
    TaskRecord {
        id,
        schema_version: 1,
        kind: TaskKind::Generic,
        status,
        user_id: "user_task".to_string(),
        child_id: None,
        api_key_id: "ak_task".to_string(),
        client_protocol: "openai".to_string(),
        vendor_id: "vendor_task".to_string(),
        vendor_key_hint: None,
        model: Some("gpt-task".to_string()),
        created_at_ms: 1_000,
        updated_at_ms: 1_000,
        submitted_ts_ms: 1_000,
        expires_at_ms: 87_400_000,
        claimed_ts_ms: None,
        ended_ts_ms: None,
        lease_until_ms: None,
        worker_id: None,
        trace_id: Some("trace_task".to_string()),
        request_id: Some("request_task".to_string()),
        provider_task_id: None,
        provider_first_response_ms: None,
        input_payload: serde_json::json!({"model":"gpt-task"}),
        output_payload: None,
        error_message: None,
        frozen_micro: 0,
        actual_micro: None,
        duration_seconds: None,
        billing_record_id: None,
    }
}

type FreezeCalls = Arc<Mutex<Vec<(String, TaskId, i64, Option<i64>)>>>;
type ReleaseCalls = Arc<Mutex<Vec<(String, TaskId, i64)>>>;

#[derive(Clone, Default)]
struct MemoryTaskContract {
    submitted: Arc<Mutex<Vec<TaskRecord>>>,
    records: Arc<Mutex<std::collections::HashMap<TaskId, TaskRecord>>>,
    cancelled: Arc<Mutex<Vec<(TaskId, CancelReason)>>>,
    freezes: FreezeCalls,
    releases: ReleaseCalls,
}

impl MemoryTaskContract {
    fn with_record(record: TaskRecord) -> Self {
        let this = Self::default();
        this.records.lock().unwrap().insert(record.id, record);
        this
    }
}

impl TaskContract for MemoryTaskContract {
    async fn freeze_task(
        &self,
        user_id: &str,
        task_id: &TaskId,
        amount: i64,
        available_balance_micro: Option<i64>,
    ) -> Result<(), TaskError> {
        self.freezes.lock().unwrap().push((
            user_id.to_string(),
            *task_id,
            amount,
            available_balance_micro,
        ));
        if available_balance_micro.is_some_and(|balance| balance < amount) {
            return Err(TaskError::QuotaExceeded);
        }
        Ok(())
    }

    async fn release_task_freeze(
        &self,
        user_id: &str,
        task_id: &TaskId,
        amount: i64,
    ) -> Result<(), TaskError> {
        self.releases
            .lock()
            .unwrap()
            .push((user_id.to_string(), *task_id, amount));
        Ok(())
    }

    async fn submit(&self, record: TaskRecord) -> Result<TaskId, TaskError> {
        let id = record.id;
        self.submitted.lock().unwrap().push(record.clone());
        self.records.lock().unwrap().insert(id, record);
        Ok(id)
    }

    async fn load(&self, id: &TaskId) -> Result<Option<TaskRecord>, TaskError> {
        Ok(self.records.lock().unwrap().get(id).cloned())
    }

    async fn watch(&self, id: &TaskId, _wait_ms: u64) -> Result<Option<TaskRecord>, TaskError> {
        self.load(id).await
    }

    async fn request_cancel(
        &self,
        id: &TaskId,
        reason: CancelReason,
    ) -> Result<TaskRecord, TaskError> {
        self.cancelled.lock().unwrap().push((*id, reason));
        let mut records = self.records.lock().unwrap();
        let mut record = records.get(id).cloned().ok_or(TaskError::NotFound(*id))?;
        record.status = TaskStatus::Cancelling;
        records.insert(*id, record.clone());
        Ok(record)
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
        let mut records = self.records.lock().unwrap();
        let mut record = records.get(id).cloned().ok_or(TaskError::NotFound(*id))?;
        record.status = TaskStatus::Running;
        record.lease_until_ms = Some(lease_until_ms);
        record.provider_task_id = provider_task_id;
        records.insert(*id, record.clone());
        Ok(record)
    }

    async fn settle(
        &self,
        id: &TaskId,
        terminal_status: TaskStatus,
        output_payload: Option<serde_json::Value>,
        error_message: Option<String>,
        actual_micro: i64,
    ) -> Result<SettleOutcome, TaskError> {
        let mut records = self.records.lock().unwrap();
        let mut record = records.get(id).cloned().ok_or(TaskError::NotFound(*id))?;
        record.status = terminal_status;
        record.output_payload = output_payload;
        record.error_message = error_message;
        record.actual_micro = Some(actual_micro);
        records.insert(*id, record.clone());
        Ok(SettleOutcome {
            record,
            released_micro: 0,
            deducted_micro: actual_micro,
            billing_record_id: None,
        })
    }

    async fn recover_lease(&self, id: &TaskId, _now_ms: u64) -> Result<TaskRecord, TaskError> {
        self.load(id).await?.ok_or(TaskError::NotFound(*id))
    }
}

#[tokio::test]
async fn test_submit_endpoint_creates_task() {
    let contract = MemoryTaskContract::default();

    let response = gw_proxy::filters::task::submit_endpoint(
        &contract,
        gw_proxy::filters::task::SubmitTaskRequest {
            body: br#"{"model":"gpt-task","input":"draw"}"#,
            user_id: "user_task",
            api_key_id: Some("ak_task"),
            client_protocol: "openai",
            client_codec: None,
            trace_id: "trace_task",
            request_id: "request_task",
            billing_config: None,
            available_balance_micro: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.status, 201);
    assert_eq!(response.body["status"], "queued");
    assert!(response.body["task_id"].as_str().is_some());
    assert!(response.body["expires_at"].as_u64().is_some());
    assert_eq!(contract.submitted.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn test_freeze_guard_freezes_on_submit() {
    let contract = MemoryTaskContract::default();
    let user_id = "user_task_freeze";
    seed_balance(user_id, 10_000);

    let response = gw_proxy::filters::task::submit_endpoint(
        &contract,
        gw_proxy::filters::task::SubmitTaskRequest {
            body: br#"{"model":"gpt-task","input":"draw"}"#,
            user_id,
            api_key_id: Some("ak_task"),
            client_protocol: "openai",
            client_codec: None,
            trace_id: "trace_task",
            request_id: "request_task",
            billing_config: Some(TaskBillingConfig {
                base_micro: 2_000,
                vendor_multiplier: 1.0,
                estimated_duration_secs: 30,
            }),
            available_balance_micro: Some(10_000),
        },
    )
    .await
    .unwrap();

    assert_eq!(response.status, 201);
    let submitted = contract.submitted.lock().unwrap();
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].frozen_micro, 2_000);
    assert_eq!(
        contract.freezes.lock().unwrap().as_slice(),
        &[(user_id.to_string(), submitted[0].id, 2_000, Some(10_000))]
    );
    assert_eq!(
        frozen_balance(user_id),
        0,
        "task submit must use Redis-backed contract freeze, not process-local DEDUCT"
    );
}

#[tokio::test]
async fn test_freeze_guard_insufficient_balance() {
    let contract = MemoryTaskContract::default();
    let user_id = "user_task_insufficient";
    seed_balance(user_id, 1_000);

    let err = gw_proxy::filters::task::submit_endpoint(
        &contract,
        gw_proxy::filters::task::SubmitTaskRequest {
            body: br#"{"model":"gpt-task","input":"draw"}"#,
            user_id,
            api_key_id: Some("ak_task"),
            client_protocol: "openai",
            client_codec: None,
            trace_id: "trace_task",
            request_id: "request_task",
            billing_config: Some(TaskBillingConfig {
                base_micro: 2_000,
                vendor_multiplier: 1.0,
                estimated_duration_secs: 30,
            }),
            available_balance_micro: Some(1_000),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.status, 402);
    assert_eq!(err.code, gw_core::error::codes::GW_TASK_FREEZE_FAILED);
    assert_eq!(contract.submitted.lock().unwrap().len(), 0);
    assert_eq!(frozen_balance(user_id), 0);
}

#[tokio::test]
async fn test_load_endpoint_returns_task() {
    let id = TaskId::new_v4();
    let contract = MemoryTaskContract::with_record(task_record(id, TaskStatus::Queued));

    let response = gw_proxy::filters::task::load_endpoint(&contract, &id.to_string())
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body["id"], id.to_string());
    assert_eq!(response.body["status"], "queued");
}

#[tokio::test]
async fn test_cancel_endpoint_requests_cancel() {
    let id = TaskId::new_v4();
    let contract = MemoryTaskContract::with_record(task_record(id, TaskStatus::Running));

    let response = gw_proxy::filters::task::cancel_endpoint(&contract, &id.to_string())
        .await
        .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body["status"], "cancelling");
    assert_eq!(
        contract.cancelled.lock().unwrap().as_slice(),
        &[(id, CancelReason::Client)]
    );
}

#[tokio::test]
async fn test_load_nonexistent_task() {
    let id = TaskId::new_v4();
    let contract = MemoryTaskContract::default();

    let err = gw_proxy::filters::task::load_endpoint(&contract, &id.to_string())
        .await
        .unwrap_err();

    assert_eq!(err.status, 404);
    assert_eq!(err.code, gw_core::error::codes::GW_TASK_NOT_FOUND);
}

#[tokio::test]
async fn test_submit_endpoint_stores_codec_rendered_full_ir_wire_body() {
    let contract = MemoryTaskContract::default();

    let response = gw_proxy::filters::task::submit_endpoint(
        &contract,
        gw_proxy::filters::task::SubmitTaskRequest {
            body: br#"{"upstream_url":"https://example.invalid/v1/chat/completions","body":{"model":"gpt-4.1-mini","messages":[{"role":"user","content":"hi"}]}}"#,
            user_id: "user_task",
            api_key_id: Some("ak_task"),
            client_protocol: "openai:chat",
            client_codec: Some(&AnyCodec::OpenAi(gw_codec::openai::OpenAi)),
            trace_id: "trace_task",
            request_id: "request_task",
            billing_config: None,
            available_balance_micro: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.status, 201);
    let submitted = contract.submitted.lock().unwrap();
    let payload = &submitted[0].input_payload;
    assert_eq!(payload["ir_wire_format"], "openai:chat");
    assert_eq!(payload["ir_request"]["protocol"], "openai:chat");
    assert_eq!(payload["ir_request"]["kind"], "request");
    assert_eq!(
        payload["ir_request"]["value"]["messages"][0]["content"]["value"],
        "hi"
    );
    assert_eq!(payload["ir_wire_content_type"], "application/json");
    let rendered = payload["ir_wire_body"].as_str().expect("rendered body");
    let rendered_json: serde_json::Value =
        serde_json::from_str(rendered).expect("rendered body is json");
    assert_eq!(rendered_json["model"], "gpt-4.1-mini");
    assert_eq!(rendered_json["messages"][0]["content"], "hi");
    assert_eq!(payload["body"]["messages"][0]["content"], "hi");
}

#[tokio::test]
async fn test_submit_endpoint_skips_ir_attachment_for_cli_protocol_with_responses_codec() {
    let contract = MemoryTaskContract::default();

    let response = gw_proxy::filters::task::submit_endpoint(
        &contract,
        gw_proxy::filters::task::SubmitTaskRequest {
            body: br#"{"model":"gpt-5","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}],"metadata":{"cli":"codex"}}"#,
            user_id: "user_task",
            api_key_id: Some("ak_task"),
            client_protocol: "openai:cli",
            client_codec: Some(&AnyCodec::OpenAiResponses(
                gw_codec::openai::responses::OpenAiResponsesCodec,
            )),
            trace_id: "trace_task",
            request_id: "request_task",
            billing_config: None,
            available_balance_micro: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.status, 201);
    let submitted = contract.submitted.lock().unwrap();
    assert_eq!(submitted.len(), 1);
    let payload = &submitted[0].input_payload;
    assert_eq!(submitted[0].client_protocol, "openai:cli");
    assert_eq!(payload["model"], "gpt-5");
    assert_eq!(payload["metadata"]["cli"], "codex");
    assert!(payload.get("ir_request").is_none());
    assert!(payload.get("ir_wire_body").is_none());
    assert!(payload.get("ir_wire_format").is_none());
}

#[tokio::test]
async fn test_submit_endpoint_preserves_direct_submit_for_cli_body_marker() {
    let contract = MemoryTaskContract::default();

    let response = gw_proxy::filters::task::submit_endpoint(
        &contract,
        gw_proxy::filters::task::SubmitTaskRequest {
            body: br#"{"model":"gpt-5","background":true,"input":"hi","metadata":{"cli":"codex"}}"#,
            user_id: "user_task",
            api_key_id: Some("ak_task"),
            client_protocol: "openai:cli",
            client_codec: Some(&AnyCodec::OpenAiResponses(
                gw_codec::openai::responses::OpenAiResponsesCodec,
            )),
            trace_id: "trace_task",
            request_id: "request_task",
            billing_config: None,
            available_balance_micro: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.status, 201);
    let submitted = contract.submitted.lock().unwrap();
    let payload = &submitted[0].input_payload;
    assert_eq!(submitted[0].client_protocol, "openai:cli");
    assert_eq!(payload["background"], true);
    assert_eq!(payload["metadata"]["cli"], "codex");
    assert!(payload.get("ir_request").is_none());
    assert!(payload.get("ir_wire_body").is_none());
    assert!(payload.get("ir_wire_format").is_none());
}

#[tokio::test]
async fn test_submit_endpoint_stores_full_ir_for_responses_background() {
    let contract = MemoryTaskContract::default();

    let response = gw_proxy::filters::task::submit_endpoint(
        &contract,
        gw_proxy::filters::task::SubmitTaskRequest {
            body: br#"{"model":"gpt-5","background":true,"input":"hi"}"#,
            user_id: "user_task",
            api_key_id: Some("ak_task"),
            client_protocol: "openai:responses",
            client_codec: Some(&AnyCodec::OpenAiResponses(
                gw_codec::openai::responses::OpenAiResponsesCodec,
            )),
            trace_id: "trace_task",
            request_id: "request_task",
            billing_config: None,
            available_balance_micro: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.status, 201);
    let submitted = contract.submitted.lock().unwrap();
    let payload = &submitted[0].input_payload;
    assert_eq!(submitted[0].client_protocol, "openai:responses");
    assert_eq!(payload["ir_wire_format"], "openai:responses");
    assert_eq!(payload["ir_request"]["protocol"], "openai:responses");
    assert_eq!(payload["ir_request"]["kind"], "request");
    assert_eq!(
        payload["ir_request"]["value"]["messages"][0]["content"]["value"],
        "hi"
    );
    let rendered = payload["ir_wire_body"].as_str().expect("rendered body");
    let rendered_json: serde_json::Value =
        serde_json::from_str(rendered).expect("rendered body is json");
    assert_eq!(rendered_json["model"], "gpt-5");
    assert_eq!(rendered_json["background"], true);
    assert_eq!(rendered_json["input"], "hi");
}

#[tokio::test]
async fn test_submit_endpoint_attaches_responses_ir_for_direct_submit_without_cli_body_marker() {
    let contract = MemoryTaskContract::default();

    let response = gw_proxy::filters::task::submit_endpoint(
        &contract,
        gw_proxy::filters::task::SubmitTaskRequest {
            body: br#"{"model":"gpt-5","background":true,"input":"hi"}"#,
            user_id: "user_task",
            api_key_id: Some("ak_task"),
            client_protocol: "openai:cli",
            client_codec: Some(&AnyCodec::OpenAiResponses(
                gw_codec::openai::responses::OpenAiResponsesCodec,
            )),
            trace_id: "trace_task",
            request_id: "request_task",
            billing_config: None,
            available_balance_micro: None,
        },
    )
    .await
    .unwrap();

    assert_eq!(response.status, 201);
    let submitted = contract.submitted.lock().unwrap();
    let payload = &submitted[0].input_payload;
    assert_eq!(submitted[0].client_protocol, "openai:cli");
    assert_eq!(payload["ir_wire_format"], "openai:responses");
    assert_eq!(payload["ir_request"]["protocol"], "openai:responses");
    let rendered = payload["ir_wire_body"].as_str().expect("rendered body");
    let rendered_json: serde_json::Value =
        serde_json::from_str(rendered).expect("rendered body is json");
    assert_eq!(rendered_json["background"], true);
    assert_eq!(rendered_json["input"], "hi");
}

fn seed_balance(user_id: &str, base_micro: i64) {
    let state = Arc::new(DeductionState {
        base_micro: AtomicI64::new(base_micro),
        deducted_micro: AtomicI64::new(0),
        frozen_micro: AtomicI64::new(0),
    });
    DEDUCT.insert(UserId::from(user_id), state);
}

fn frozen_balance(user_id: &str) -> i64 {
    DEDUCT
        .get(&UserId::from(user_id))
        .map(|state| state.frozen_micro.load(Ordering::SeqCst))
        .unwrap_or_default()
}
