use std::collections::VecDeque;
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
    poll_count: Arc<Mutex<usize>>,
    responses: Arc<Mutex<VecDeque<UpstreamResponse>>>,
}

impl MemoryTransport {
    fn with_responses(responses: Vec<UpstreamResponse>) -> Self {
        Self {
            requests: Arc::default(),
            poll_count: Arc::default(),
            responses: Arc::new(Mutex::new(VecDeque::from(responses))),
        }
    }

    fn with_response(response: UpstreamResponse) -> Self {
        Self::with_responses(vec![response])
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn poll_count(&self) -> usize {
        *self.poll_count.lock().unwrap()
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
            .ok_or_else(|| TransportError::Failed("missing mock response".to_string()))
    }

    async fn poll(
        &self,
        _ctx: gw_task_runtime::template_executor::PollContext,
    ) -> Result<UpstreamResponse, TransportError> {
        *self.poll_count.lock().unwrap() += 1;
        self.responses
            .lock()
            .unwrap()
            .pop_front()
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
        task_kind: serde_json::from_value(fixture["profile"]["task_kind"].clone())
            .unwrap_or(TaskKind::Image),
        client_mode,
        upstream_mode,
        profile,
        binding: fixture["binding"].clone(),
        client_request: fixture["client_request"].clone(),
        task: json!({"id": "task_1", "status": "queued"}),
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
async fn sync_async_lro_polls_and_returns_final_response() {
    let transport = MemoryTransport::with_responses(vec![
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({"name": "operations/video-123"}),
        },
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({
                "name": "operations/video-123",
                "done": true,
                "response": {"videos": [{"uri": "gs://bucket/video-123.mp4"}]}
            }),
        },
    ]);
    let executor = TemplateExecutor::new(Arc::new(transport.clone()));

    let result = executor
        .execute_inline(fixture_request(
            "custom-lro-google-veo.json",
            ClientMode::Sync,
            UpstreamMode::AsyncLro,
        ))
        .await
        .expect("execute inline lro");

    match result {
        ExecutionResult::Completed { response, events } => {
            assert_eq!(response["data"][0]["url"], "gs://bucket/video-123.mp4");
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
    assert_eq!(transport.request_count(), 1);
    assert_eq!(transport.poll_count(), 1);
}

#[tokio::test]
async fn async_sync_inline_returns_task_reference_without_upstream_call() {
    let transport = MemoryTransport::with_response(UpstreamResponse {
        status: 200,
        headers: Default::default(),
        body: json!({"data": [{"b64_json": "unused"}]}),
    });
    let executor = TemplateExecutor::new(Arc::new(transport.clone()));

    let result = executor
        .execute_inline(fixture_request(
            "openai-image-default.json",
            ClientMode::Async,
            UpstreamMode::Sync,
        ))
        .await
        .expect("execute inline async client");

    match result {
        ExecutionResult::Submitted { response, events } => {
            assert_eq!(response["id"], "task_1");
            assert_eq!(response["object"], "task");
            assert_eq!(response["status"], "queued");
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.status.as_str())
                    .collect::<Vec<_>>(),
                ["submitted"]
            );
        }
        other => panic!("expected submitted result, got {other:?}"),
    }
    assert_eq!(transport.request_count(), 0);
    assert_eq!(transport.poll_count(), 0);
}

#[tokio::test]
async fn async_sync_background_executes_to_completion() {
    let transport = MemoryTransport::with_response(UpstreamResponse {
        status: 200,
        headers: Default::default(),
        body: json!({"data": [{"b64_json": "aW1hZ2U="}]}),
    });
    let executor = TemplateExecutor::new(Arc::new(transport.clone()));

    let result = executor
        .execute_background(fixture_request(
            "openai-image-default.json",
            ClientMode::Async,
            UpstreamMode::Sync,
        ))
        .await
        .expect("execute async background");

    match result {
        ExecutionResult::Completed { response, .. } => {
            assert_eq!(response["data"][0]["b64_json"], "aW1hZ2U=");
        }
        other => panic!("expected completed result, got {other:?}"),
    }
    assert_eq!(transport.request_count(), 1);
}

#[tokio::test]
async fn async_async_lro_background_polls_to_completion() {
    let transport = MemoryTransport::with_responses(vec![
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({"name": "operations/video-456"}),
        },
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({
                "name": "operations/video-456",
                "done": true,
                "response": {"videos": [{"uri": "gs://bucket/video-456.mp4"}]}
            }),
        },
    ]);
    let executor = TemplateExecutor::new(Arc::new(transport.clone()));

    let result = executor
        .execute_background(fixture_request(
            "custom-lro-google-veo.json",
            ClientMode::Async,
            UpstreamMode::AsyncLro,
        ))
        .await
        .expect("execute async lro background");

    match result {
        ExecutionResult::Completed { response, .. } => {
            assert_eq!(response["data"][0]["url"], "gs://bucket/video-456.mp4");
        }
        other => panic!("expected completed result, got {other:?}"),
    }
    assert_eq!(transport.request_count(), 1);
    assert_eq!(transport.poll_count(), 1);
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
