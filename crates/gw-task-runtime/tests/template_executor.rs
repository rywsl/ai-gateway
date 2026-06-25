use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use gw_task::template::TemplateError;
use gw_task::{ClientMode, TaskId, TaskKind, TaskRecord, TaskStatus, UpstreamMode};
use gw_task_runtime::template_executor::{
    execution_request_from_task_record, ExecutionIdentity, ExecutionRequest, ExecutionResult,
    TemplateExecutor, TransportError, UpstreamRequest, UpstreamResponse, UpstreamTransport,
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
        "{}/../../../AILinkMax-Go/docs/contracts/async-task-template/v1/fixtures/{}",
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

fn template_task_record() -> TaskRecord {
    let now = 1_700_000_000_000;
    TaskRecord {
        id: TaskId::new_v4(),
        schema_version: 2,
        kind: TaskKind::Image,
        status: TaskStatus::Queued,
        user_id: "user_1".to_string(),
        child_id: None,
        api_key_id: "ak_1".to_string(),
        vendor_id: "openai".to_string(),
        model: Some("gpt-image-2".to_string()),
        client_mode: ClientMode::Async,
        upstream_mode: UpstreamMode::Sync,
        global_model_id: Some("gpt-image-2".to_string()),
        provider_model_id: Some("gpt-image-2".to_string()),
        endpoint_id: Some("endpoint_openai".to_string()),
        provider_key_id: Some("key_openai".to_string()),
        profile_id: Some("profile_image".to_string()),
        profile_revision_id: Some("profile_image:v1".to_string()),
        profile_version: Some(1),
        binding_id: Some("binding_image".to_string()),
        binding_revision_id: Some("binding_image:v1".to_string()),
        binding_version: Some(1),
        effective_template_hash: Some("sha256:image".to_string()),
        poll_context: None,
        created_at_ms: now,
        updated_at_ms: now,
        submitted_ts_ms: now,
        expires_at_ms: now + 86_400_000,
        claimed_ts_ms: None,
        ended_ts_ms: None,
        lease_until_ms: None,
        worker_id: None,
        trace_id: Some("trace_1".to_string()),
        request_id: Some("req_1".to_string()),
        provider_task_id: None,
        provider_first_response_ms: None,
        input_payload: serde_json::json!({
            "client_request": {"model": "gpt-image-2", "prompt": "glass sphere"},
            "profile": {
                "id": "profile_image",
                "task_kind": "image",
                "client_modes": ["async"],
                "default_client_mode": "async",
                "upstream_mode": "sync",
                "client_request_template": {"normalize": {"client.model": "$.model", "client.prompt": "$.prompt"}},
                "upstream_request_template": {
                    "method": "POST",
                    "path": "/v1/images/generations",
                    "body": {"model": "{{binding.provider_model_name}}", "prompt": "{{client.prompt}}"}
                },
                "upstream_response_template": {"result": {"images": "$.data[*].b64_json"}},
                "client_sync_response_template": {
                    "data": {
                        "$map": {
                            "source": "{{result.images}}",
                            "as": "image",
                            "template": {"b64_json": "{{image}}"}
                        }
                    }
                }
            },
            "binding": {
                "id": "binding_image",
                "global_model_id": "gpt-image-2",
                "provider_model_name": "gpt-image-2"
            }
        }),
        output_payload: None,
        error_message: None,
        frozen_micro: 0,
        actual_micro: None,
        duration_seconds: None,
        billing_record_id: None,
    }
}

#[test]
fn execution_request_from_task_record_rehydrates_template_snapshot() {
    let record = template_task_record();
    let request = execution_request_from_task_record(&record).expect("execution request");

    assert_eq!(request.identity.user_id, "user_1");
    assert_eq!(request.identity.api_key_id, "ak_1");
    assert_eq!(request.identity.request_id, "req_1");
    assert_eq!(request.task_kind, TaskKind::Image);
    assert_eq!(request.client_mode, ClientMode::Async);
    assert_eq!(request.upstream_mode, UpstreamMode::Sync);
    assert_eq!(request.profile["id"], "profile_image");
    assert_eq!(request.binding["id"], "binding_image");
    assert_eq!(request.client_request["prompt"], "glass sphere");
    assert_eq!(request.task["id"], record.id.to_string());
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
    assert_eq!(transport.request_count(), 2);
    assert_eq!(transport.poll_count(), 0);
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, "/v1/operations/video-123");
}

#[tokio::test]
async fn sync_async_lro_uses_openai_style_poll_template_status_mapping() {
    let request = ExecutionRequest {
        identity: ExecutionIdentity {
            user_id: "user_1".to_string(),
            api_key_id: "ak_1".to_string(),
            request_id: "req_1".to_string(),
            trace_id: "trace_1".to_string(),
        },
        task_kind: TaskKind::Video,
        client_mode: ClientMode::Sync,
        upstream_mode: UpstreamMode::AsyncLro,
        profile: json!({
            "id": "profile_video",
            "task_kind": "video",
            "client_modes": ["sync"],
            "default_client_mode": "sync",
            "upstream_mode": "async_lro",
            "client_request_template": {
                "normalize": {
                    "client.model": "$.model",
                    "client.prompt": "$.prompt"
                }
            },
            "upstream_request_template": {
                "method": "POST",
                "path": "/v1/videos",
                "headers": {"Content-Type": "application/json"},
                "body": {
                    "model": "{{binding.provider_model_name}}",
                    "prompt": "{{client.prompt}}"
                }
            },
            "upstream_poll_template": {
                "method": "GET",
                "path": "/v1/videos/{{result.provider_task_id}}",
                "headers": {"X-Poll": "{{result.provider_task_id}}"},
                "status": {
                    "path": "$.status",
                    "pending": ["queued", "in_progress"],
                    "succeeded": ["completed"],
                    "failed": ["failed", "cancelled"]
                },
                "provider_task_id": "$.id"
            },
            "upstream_response_template": {
                "result": {
                    "video_id": "$.id",
                    "status": "$.status",
                    "url": "$.content_url"
                }
            },
            "client_sync_response_template": {
                "id": "{{result.video_id}}",
                "object": "video",
                "status": "{{result.status}}",
                "url": "{{result.url}}"
            }
        }),
        binding: json!({
            "id": "binding_video",
            "provider_model_name": "sora-2"
        }),
        client_request: json!({
            "model": "sora-2",
            "prompt": "city at night"
        }),
        task: json!({"id": "task_1", "status": "queued"}),
    };
    let transport = MemoryTransport::with_responses(vec![
        UpstreamResponse {
            status: 202,
            headers: Default::default(),
            body: json!({"id": "video_123", "status": "queued"}),
        },
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({
                "id": "video_123",
                "status": "completed",
                "content_url": "https://example.test/video.mp4"
            }),
        },
    ]);
    let executor = TemplateExecutor::new(Arc::new(transport.clone()));

    let result = executor
        .execute_inline(request)
        .await
        .expect("execute openai video poll");

    match result {
        ExecutionResult::Completed { response, .. } => {
            assert_eq!(response["id"], "video_123");
            assert_eq!(response["status"], "completed");
            assert_eq!(response["url"], "https://example.test/video.mp4");
        }
        other => panic!("expected completed result, got {other:?}"),
    }
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, "/v1/videos/video_123");
    assert_eq!(requests[1].headers["X-Poll"], "video_123");
}

#[tokio::test]
async fn sync_async_lro_records_openai_poll_after_header_as_wait_event() {
    let request = ExecutionRequest {
        identity: ExecutionIdentity {
            user_id: "user_1".to_string(),
            api_key_id: "ak_1".to_string(),
            request_id: "req_1".to_string(),
            trace_id: "trace_1".to_string(),
        },
        task_kind: TaskKind::Video,
        client_mode: ClientMode::Sync,
        upstream_mode: UpstreamMode::AsyncLro,
        profile: json!({
            "id": "profile_video",
            "task_kind": "video",
            "client_modes": ["sync"],
            "default_client_mode": "sync",
            "upstream_mode": "async_lro",
            "client_request_template": {"normalize": {"client.model": "$.model"}},
            "upstream_request_template": {
                "method": "POST",
                "path": "/v1/videos",
                "body": {"model": "{{binding.provider_model_name}}"}
            },
            "upstream_poll_template": {
                "method": "GET",
                "path": "/v1/videos/{{result.provider_task_id}}",
                "status": {
                    "path": "$.status",
                    "pending": ["queued", "in_progress"],
                    "succeeded": ["completed"],
                    "failed": ["failed"]
                },
                "provider_task_id": "$.id"
            },
            "upstream_response_template": {"result": {"video_id": "$.id"}},
            "client_sync_response_template": {"id": "{{result.video_id}}"}
        }),
        binding: json!({"provider_model_name": "sora-2"}),
        client_request: json!({"model": "sora-2"}),
        task: json!({"id": "task_1", "status": "queued"}),
    };
    let transport = MemoryTransport::with_responses(vec![
        UpstreamResponse {
            status: 202,
            headers: Default::default(),
            body: json!({"id": "video_123", "status": "queued"}),
        },
        UpstreamResponse {
            status: 200,
            headers: [("openai-poll-after-ms".to_string(), "250".to_string())].into(),
            body: json!({"id": "video_123", "status": "queued"}),
        },
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({"id": "video_123", "status": "completed"}),
        },
    ]);
    let executor = TemplateExecutor::new(Arc::new(transport));

    let result = executor
        .execute_inline(request)
        .await
        .expect("execute openai video poll");

    match result {
        ExecutionResult::Completed { events, .. } => {
            assert_eq!(
                events
                    .iter()
                    .filter_map(|event| event.poll_wait_ms)
                    .collect::<Vec<_>>(),
                vec![250]
            );
        }
        other => panic!("expected completed result, got {other:?}"),
    }
}

#[tokio::test]
async fn sync_async_lro_waits_between_pending_poll_attempts() {
    let mut request = fixture_request(
        "custom-lro-google-veo.json",
        ClientMode::Sync,
        UpstreamMode::AsyncLro,
    );
    request.profile["upstream_poll_template"]["poll_interval_ms"] = json!(25);
    let transport = MemoryTransport::with_responses(vec![
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({"name": "operations/video-789"}),
        },
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({"name": "operations/video-789", "done": false}),
        },
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({
                "name": "operations/video-789",
                "done": true,
                "response": {"videos": [{"uri": "gs://bucket/video-789.mp4"}]}
            }),
        },
    ]);
    let executor = TemplateExecutor::new(Arc::new(transport));

    let started = Instant::now();
    let result = executor.execute_inline(request).await.expect("execute lro");

    assert!(matches!(result, ExecutionResult::Completed { .. }));
    assert!(
        started.elapsed() >= Duration::from_millis(45),
        "poll loop should clamp very small intervals before pending attempts"
    );
}

#[tokio::test]
async fn sync_async_lro_uses_default_poll_wait_when_unconfigured() {
    let mut request = fixture_request(
        "custom-lro-google-veo.json",
        ClientMode::Sync,
        UpstreamMode::AsyncLro,
    );
    request.profile["upstream_poll_template"]
        .as_object_mut()
        .expect("poll template object")
        .remove("poll_interval_ms");
    let transport = MemoryTransport::with_responses(vec![
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({"name": "operations/video-789"}),
        },
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({"name": "operations/video-789", "done": false}),
        },
        UpstreamResponse {
            status: 200,
            headers: Default::default(),
            body: json!({
                "name": "operations/video-789",
                "done": true,
                "response": {"videos": [{"uri": "gs://bucket/video-789.mp4"}]}
            }),
        },
    ]);
    let executor = TemplateExecutor::new(Arc::new(transport));

    let started = Instant::now();
    let result = executor
        .execute_inline(request)
        .await
        .expect("execute lro with default wait");

    assert!(matches!(result, ExecutionResult::Completed { .. }));
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "poll loop should use a non-zero default wait between pending attempts"
    );
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
    assert_eq!(transport.request_count(), 2);
    assert_eq!(transport.poll_count(), 0);
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
