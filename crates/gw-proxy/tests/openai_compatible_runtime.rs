use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gw_proxy::filters::task::{
    handle_openai_compatible_submit, submit_openai_compatible_task, ResolvedExecutionModes,
    ResolvedTaskRuntime,
};
use gw_task::{ClientMode, TaskContract, TaskError, TaskId, TaskRecord, UpstreamMode};
use gw_task_runtime::template_executor::{
    PollContext, TransportError, UpstreamRequest, UpstreamResponse, UpstreamTransport,
};

#[derive(Clone)]
struct MemoryTransport {
    requests: Arc<Mutex<Vec<UpstreamRequest>>>,
    responses: Arc<Mutex<VecDeque<UpstreamResponse>>>,
}

impl MemoryTransport {
    fn new(response: serde_json::Value) -> Self {
        Self {
            requests: Arc::default(),
            responses: Arc::new(Mutex::new(VecDeque::from([UpstreamResponse {
                status: 200,
                headers: Default::default(),
                body: response,
            }]))),
        }
    }
}

#[async_trait]
impl UpstreamTransport for MemoryTransport {
    async fn submit(&self, request: UpstreamRequest) -> Result<UpstreamResponse, TransportError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| TransportError::Failed("missing response".to_string()))
    }

    async fn poll(&self, _ctx: PollContext) -> Result<UpstreamResponse, TransportError> {
        Err(TransportError::Failed("poll not expected".to_string()))
    }
}

fn image_runtime() -> ResolvedTaskRuntime {
    ResolvedTaskRuntime {
        profile: serde_json::json!({
            "id": "profile-image",
            "task_kind": "image",
            "client_modes": ["sync"],
            "default_client_mode": "sync",
            "upstream_mode": "sync",
            "client_request_template": {
                "normalize": {
                    "client.model": "$.model",
                    "client.prompt": "$.prompt",
                    "client.n": {"value": "$.n", "default": 1}
                }
            },
            "upstream_request_template": {
                "method": "POST",
                "path": "/v1/images/generations",
                "headers": {"Content-Type": "application/json"},
                "body": {
                    "model": "{{binding.provider_model_name}}",
                    "prompt": "{{client.prompt}}",
                    "n": {"$expr": "client.n", "as": "number"}
                }
            },
            "upstream_response_template": {
                "result": {"images": "$.data[*].b64_json"}
            },
            "client_sync_response_template": {
                "created": 1710000000,
                "data": {
                    "$map": {
                        "source": "{{result.images}}",
                        "as": "image",
                        "template": {"b64_json": "{{image}}"}
                    }
                }
            },
            "client_async_submit_response_template": {
                "id": "{{task.id}}",
                "object": "task",
                "status": "{{task.status}}",
                "model": "{{client.model}}"
            }
        }),
        binding: serde_json::json!({
            "id": "binding-image",
            "global_model_id": "gpt-image-2",
            "provider_model_id": "pm-gpt-image-2",
            "provider_model_name": "gpt-image-2"
        }),
        profile_revision_id: "profile-image:v1".to_string(),
        binding_revision_id: "binding-image:v1".to_string(),
        effective_template_hash: "sha256:image".to_string(),
        upstream_base_url: None,
    }
}

#[derive(Clone, Default)]
struct CapturingTaskContract {
    submitted: Arc<Mutex<Vec<TaskRecord>>>,
}

impl TaskContract for CapturingTaskContract {
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
        self.submitted.lock().unwrap().push(record);
        Ok(id)
    }

    async fn load(&self, _id: &TaskId) -> Result<Option<TaskRecord>, TaskError> {
        Ok(None)
    }

    async fn watch(&self, _id: &TaskId, _wait_ms: u64) -> Result<Option<TaskRecord>, TaskError> {
        Ok(None)
    }

    async fn request_cancel(
        &self,
        id: &TaskId,
        _reason: gw_task::CancelReason,
    ) -> Result<TaskRecord, TaskError> {
        Err(TaskError::NotFound(*id))
    }

    async fn claim(
        &self,
        _consumer: &str,
    ) -> Result<Option<gw_task::contract::ClaimedTask>, TaskError> {
        Ok(None)
    }

    async fn update_running(
        &self,
        id: &TaskId,
        _lease_until_ms: u64,
        _provider_task_id: Option<String>,
    ) -> Result<TaskRecord, TaskError> {
        Err(TaskError::NotFound(*id))
    }

    async fn settle(
        &self,
        id: &TaskId,
        _terminal_status: gw_task::TaskStatus,
        _output_payload: Option<serde_json::Value>,
        _error_message: Option<String>,
        _actual_micro: i64,
    ) -> Result<gw_task::contract::SettleOutcome, TaskError> {
        Err(TaskError::NotFound(*id))
    }

    async fn recover_lease(&self, id: &TaskId, _now_ms: u64) -> Result<TaskRecord, TaskError> {
        Err(TaskError::NotFound(*id))
    }
}

fn embedding_runtime() -> ResolvedTaskRuntime {
    ResolvedTaskRuntime {
        profile: serde_json::json!({
            "id": "profile-embedding",
            "task_kind": "embedding",
            "client_modes": ["sync"],
            "default_client_mode": "sync",
            "upstream_mode": "sync",
            "client_request_template": {
                "normalize": {
                    "client.model": "$.model",
                    "client.input": "$.input",
                    "client.encoding_format": {"value": "$.encoding_format", "default": "float"}
                }
            },
            "upstream_request_template": {
                "method": "POST",
                "path": "/v1/embeddings",
                "headers": {"Content-Type": "application/json"},
                "body": {
                    "model": "{{binding.provider_model_name}}",
                    "input": {"$expr": "client.input"},
                    "encoding_format": "{{client.encoding_format}}"
                }
            },
            "upstream_response_template": {
                "result": {
                    "embeddings": "$.data[*].embedding",
                    "usage.prompt_tokens": "$.usage.prompt_tokens",
                    "usage.total_tokens": "$.usage.total_tokens"
                }
            },
            "client_sync_response_template": {
                "object": "list",
                "data": {
                    "$map": {
                        "source": "{{result.embeddings}}",
                        "as": "embedding",
                        "template": {"object": "embedding", "embedding": {"$expr": "embedding"}}
                    }
                },
                "model": "{{client.model}}",
                "usage": {
                    "prompt_tokens": {"$expr": "result.usage.prompt_tokens", "as": "number"},
                    "total_tokens": {"$expr": "result.usage.total_tokens", "as": "number"}
                }
            }
        }),
        binding: serde_json::json!({
            "id": "binding-embedding",
            "global_model_id": "text-embedding-3-large",
            "provider_model_id": "pm-text-embedding-3-large",
            "provider_model_name": "text-embedding-3-large"
        }),
        profile_revision_id: "profile-embedding:v1".to_string(),
        binding_revision_id: "binding-embedding:v1".to_string(),
        effective_template_hash: "sha256:embedding".to_string(),
        upstream_base_url: None,
    }
}

#[tokio::test]
async fn sync_image_submit_renders_openai_compatible_response_without_codec() {
    let transport = MemoryTransport::new(serde_json::json!({
        "data": [{"b64_json": "Zmlyc3Q="}]
    }));

    let response = handle_openai_compatible_submit(
        image_runtime(),
        ResolvedExecutionModes {
            client_mode: ClientMode::Sync,
            upstream_mode: UpstreamMode::Sync,
        },
        serde_json::json!({
            "model": "gpt-image-2",
            "prompt": "glass sphere",
            "n": 1
        }),
        Arc::new(transport.clone()),
        "user-1",
        Some("ak-1"),
        "trace-1",
        "request-1",
    )
    .await
    .expect("response");

    assert_eq!(response.status, 200);
    assert_eq!(response.body["created"], 1710000000);
    assert_eq!(response.body["data"][0]["b64_json"], "Zmlyc3Q=");
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests[0].path, "/v1/images/generations");
    assert_eq!(requests[0].body["model"], "gpt-image-2");
}

#[tokio::test]
async fn sync_embedding_submit_renders_openai_compatible_response_without_codec() {
    let transport = MemoryTransport::new(serde_json::json!({
        "data": [{"embedding": [0.1, 0.2]}],
        "usage": {"prompt_tokens": 3, "total_tokens": 3}
    }));

    let response = handle_openai_compatible_submit(
        embedding_runtime(),
        ResolvedExecutionModes {
            client_mode: ClientMode::Sync,
            upstream_mode: UpstreamMode::Sync,
        },
        serde_json::json!({
            "model": "text-embedding-3-large",
            "input": "hello",
            "encoding_format": "float"
        }),
        Arc::new(transport),
        "user-1",
        Some("ak-1"),
        "trace-1",
        "request-1",
    )
    .await
    .expect("response");

    assert_eq!(response.status, 200);
    assert_eq!(response.body["object"], "list");
    assert_eq!(response.body["data"][0]["object"], "embedding");
    assert_eq!(response.body["data"][0]["embedding"][0], 0.1);
    assert_eq!(response.body["usage"]["total_tokens"], 3);
}

#[tokio::test]
async fn async_image_submit_persists_template_snapshots_and_returns_template_response() {
    let contract = CapturingTaskContract::default();
    let response = submit_openai_compatible_task(
        &contract,
        image_runtime(),
        ResolvedExecutionModes {
            client_mode: ClientMode::Async,
            upstream_mode: UpstreamMode::Sync,
        },
        serde_json::json!({
            "model": "gpt-image-2",
            "prompt": "glass sphere"
        }),
        "user-1",
        Some("ak-1"),
        "trace-1",
        "request-1",
        None,
        Some(10_000),
    )
    .await
    .expect("submit");

    assert_eq!(response.status, 202);
    assert_eq!(response.body["object"], "task");
    assert_eq!(response.body["status"], "queued");
    assert_eq!(response.body["model"], "gpt-image-2");

    let submitted = contract.submitted.lock().unwrap();
    assert_eq!(submitted.len(), 1);
    let record = &submitted[0];
    assert_eq!(record.client_mode, ClientMode::Async);
    assert_eq!(record.upstream_mode, UpstreamMode::Sync);
    assert_eq!(
        record.profile_revision_id.as_deref(),
        Some("profile-image:v1")
    );
    assert_eq!(
        record.binding_revision_id.as_deref(),
        Some("binding-image:v1")
    );
    assert_eq!(
        record.effective_template_hash.as_deref(),
        Some("sha256:image")
    );
    assert_eq!(
        record.input_payload["client_request"]["model"],
        "gpt-image-2"
    );
    assert_eq!(record.input_payload["profile"]["task_kind"], "image");
    assert_eq!(
        record.input_payload["binding"]["global_model_id"],
        "gpt-image-2"
    );
}
