use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use gw_task::template::TemplateError;
use gw_task::{ClientMode, TaskKind, UpstreamMode};
use gw_task_runtime::template_executor::{
    ExecutionIdentity, ExecutionRequest, ExecutionResult, TemplateExecutor, TransportError,
    UpstreamRequest, UpstreamResponse, UpstreamTransport,
};
use serde_json::json;

#[derive(Clone, Default)]
struct MemoryTransport {
    requests: Arc<Mutex<Vec<UpstreamRequest>>>,
    responses: Arc<Mutex<Vec<UpstreamResponse>>>,
}

impl MemoryTransport {
    fn with_response(response: UpstreamResponse) -> Self {
        Self {
            requests: Arc::default(),
            responses: Arc::new(Mutex::new(vec![response])),
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
            .pop()
            .ok_or_else(|| TransportError::Failed("missing mock response".to_string()))
    }

    async fn poll(
        &self,
        _ctx: gw_task_runtime::template_executor::PollContext,
    ) -> Result<UpstreamResponse, TransportError> {
        self.responses
            .lock()
            .unwrap()
            .pop()
            .ok_or_else(|| TransportError::Failed("missing mock poll response".to_string()))
    }
}

fn fixture_request(
    fixture_name: &str,
    client_mode: ClientMode,
    upstream_mode: UpstreamMode,
) -> ExecutionRequest {
    let path = format!(
        "{}/../../../async-task-template-routing-go/docs/contracts/async-task-template/v1/fixtures/{}",
        env!("CARGO_MANIFEST_DIR"),
        fixture_name
    );
    let fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture file"))
            .expect("fixture json");
    let mut profile = fixture["profile"].clone();
    profile["upstream_mode"] = serde_json::to_value(upstream_mode).expect("upstream mode");

    ExecutionRequest {
        identity: ExecutionIdentity {
            user_id: "user_1".to_string(),
            api_key_id: "ak_1".to_string(),
            request_id: "req_1".to_string(),
            trace_id: "trace_1".to_string(),
        },
        task_kind: TaskKind::Image,
        client_mode,
        upstream_mode,
        profile,
        binding: fixture["binding"].clone(),
        client_request: fixture["client_request"].clone(),
        task: json!({"id": "task_1", "status": "running"}),
    }
}

#[tokio::test]
async fn sync_sync_renders_upstream_and_client_final_response() {
    let transport = MemoryTransport::with_response(UpstreamResponse {
        status: 200,
        headers: Default::default(),
        body: json!({
            "data": [{"b64_json": "Zmlyc3Q="}, {"b64_json": "c2Vjb25k"}]
        }),
    });
    let executor = TemplateExecutor::new(Arc::new(transport.clone()));

    let result = executor
        .execute_inline(fixture_request(
            "openai-image-default.json",
            ClientMode::Sync,
            UpstreamMode::Sync,
        ))
        .await
        .expect("execute inline");

    match result {
        ExecutionResult::Completed { response, events } => {
            assert_eq!(response["object"], "list");
            assert_eq!(response["data"][0]["b64_json"], "Zmlyc3Q=");
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.status.as_str())
                    .collect::<Vec<_>>(),
                ["submitted", "running", "succeeded"]
            );
        }
        other => panic!("expected completed result, got {other:?}"),
    }
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/v1/images/generations");
    assert_eq!(requests[0].body["model"], "gpt-image-2");
}

#[tokio::test]
async fn worker_phase_one_transport_fails_explicitly_when_not_wired() {
    let executor = TemplateExecutor::new(Arc::new(
        gw_task_runtime::template_executor::NotWiredTransport,
    ));

    let err = executor
        .execute_background(fixture_request(
            "openai-image-default.json",
            ClientMode::Async,
            UpstreamMode::Sync,
        ))
        .await
        .expect_err("worker transport is not wired in phase 1");

    assert!(matches!(
        err,
        TemplateError::RenderFailed(message) if message.contains("TransportNotWired")
    ));
}
