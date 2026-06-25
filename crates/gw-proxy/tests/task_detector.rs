use gw_core::entity::AsyncTaskEndpointRegistry;
use gw_proxy::ctx::ReqCtx;
use gw_proxy::filters::task::{
    detect_openai_compatible_task_endpoint, detect_published_task_endpoint, detect_task_shape,
    mark_task_context, parse_openai_compatible_client_request, published_task_endpoint_candidate,
    resolve_task_request_key, EndpointRegistryEntry, TaskAction, TaskEndpointCandidate,
};

#[test]
fn test_task_shape_detector_identifies_task() {
    assert_eq!(
        detect_task_shape(true, "POST", "/v1/tasks", None),
        Some(TaskAction::Submit)
    );
    assert_eq!(
        detect_task_shape(true, "GET", "/v1/tasks/task_123", None),
        Some(TaskAction::Load)
    );
    assert_eq!(
        detect_task_shape(true, "GET", "/v1/tasks/task_123/watch", None),
        Some(TaskAction::Watch)
    );
    assert_eq!(
        detect_task_shape(true, "POST", "/v1/tasks/task_123/cancel", None),
        Some(TaskAction::Cancel)
    );
}

#[test]
fn test_legacy_model_paths_are_not_tasks_without_endpoint_registry() {
    assert_eq!(
        detect_task_shape(
            true,
            "POST",
            "/v1/responses",
            Some(br#"{"model":"gpt-4.1","background":true}"#)
        ),
        None
    );
    assert_eq!(
        detect_task_shape(true, "POST", "/v1/images/generations", Some(br#"{}"#)),
        None
    );
}

#[test]
fn test_published_endpoint_registry_identifies_template_task() {
    let registry = [
        EndpointRegistryEntry {
            method: "POST".to_string(),
            path: "/v1/images/generations".to_string(),
            task_kind: "image".to_string(),
            global_model_id: "gpt-image".to_string(),
        },
        EndpointRegistryEntry {
            method: "POST".to_string(),
            path: "/v1/embeddings".to_string(),
            task_kind: "embedding".to_string(),
            global_model_id: "text-embedding-3-large".to_string(),
        },
    ];

    let hit = detect_published_task_endpoint(&registry, "POST", "/v1/images/generations")
        .expect("registry hit");

    assert_eq!(hit.task_kind, "image");
    assert_eq!(hit.global_model_id, "gpt-image");
    assert!(detect_published_task_endpoint(&registry, "GET", "/v1/images/generations").is_none());
}

#[test]
fn test_published_endpoint_registry_builds_task_candidate_from_route_projection() {
    let registry = AsyncTaskEndpointRegistry {
        routes: vec![serde_json::json!({
            "method": "POST",
            "path": "/v1/videos/extensions",
            "content_type": "multipart/form-data",
            "task_kind": "video",
            "model_path": "$.model"
        })],
    };

    let candidate = published_task_endpoint_candidate(
        &registry,
        "POST",
        "/v1/videos/extensions",
        "multipart/form-data; boundary=abc",
    )
    .expect("published route candidate");

    assert_eq!(candidate.task_kind, "video");
    assert_eq!(candidate.model_path.as_deref(), Some("$.model"));
    assert_eq!(
        candidate.content_type.as_deref(),
        Some("multipart/form-data; boundary=abc")
    );
    assert!(published_task_endpoint_candidate(
        &registry,
        "POST",
        "/v1/videos/other",
        "multipart/form-data; boundary=abc",
    )
    .is_none());
}

#[test]
fn test_published_endpoint_registry_does_not_intercept_non_task_get_video_routes() {
    let registry = AsyncTaskEndpointRegistry {
        routes: vec![serde_json::json!({
            "method": "GET",
            "path": "/v1/videos/characters/char_123",
            "task_kind": "video"
        })],
    };

    assert!(published_task_endpoint_candidate(
        &registry,
        "GET",
        "/v1/videos/characters/char_123",
        "",
    )
    .is_none());
}

#[test]
fn test_openai_compatible_task_endpoint_detector_identifies_first_class_surfaces() {
    let cases = [
        (
            "POST",
            "/v1/images/generations",
            "application/json",
            "image",
            Some("$.model"),
        ),
        (
            "POST",
            "/v1/images/edits",
            "multipart/form-data; boundary=abc",
            "image",
            Some("$.model"),
        ),
        (
            "POST",
            "/v1/images/variations",
            "multipart/form-data; boundary=abc",
            "image",
            Some("$.model"),
        ),
        (
            "POST",
            "/v1/embeddings",
            "application/json",
            "embedding",
            Some("$.model"),
        ),
        (
            "POST",
            "/v1/videos",
            "multipart/form-data; boundary=abc",
            "video",
            Some("$.model"),
        ),
        (
            "POST",
            "/v1/videos/edits",
            "multipart/form-data; boundary=abc",
            "video",
            Some("$.model"),
        ),
        (
            "POST",
            "/v1/videos/extensions",
            "multipart/form-data; boundary=abc",
            "video",
            Some("$.model"),
        ),
        (
            "POST",
            "/v1/videos/video_123/remix",
            "application/json",
            "video",
            Some("$.model"),
        ),
        (
            "POST",
            "/v1/videos/characters",
            "multipart/form-data; boundary=abc",
            "video",
            Some("$.model"),
        ),
        ("GET", "/v1/videos/video_123", "", "video", None),
        ("GET", "/v1/videos/video_123/content", "", "video", None),
    ];

    for (method, path, content_type, task_kind, model_path) in cases {
        let hit = detect_openai_compatible_task_endpoint(true, method, path, content_type)
            .unwrap_or_else(|| panic!("expected endpoint candidate for {method} {path}"));
        assert_eq!(hit.method, method);
        assert_eq!(hit.path, path);
        assert_eq!(hit.task_kind, task_kind);
        assert_eq!(hit.model_path.as_deref(), model_path);
    }
}

#[test]
fn test_openai_video_resource_paths_ignore_empty_segments() {
    let cases = [
        ("GET", "/v1/videos//video_123", "video", None),
        ("GET", "/v1/videos/video_123//content", "video", None),
        (
            "POST",
            "/v1/videos//video_123/remix",
            "video",
            Some("$.model"),
        ),
    ];

    for (method, path, task_kind, model_path) in cases {
        let hit = detect_openai_compatible_task_endpoint(true, method, path, "")
            .unwrap_or_else(|| panic!("expected endpoint candidate for {method} {path}"));
        assert_eq!(hit.task_kind, task_kind);
        assert_eq!(hit.model_path.as_deref(), model_path);
    }
}

#[test]
fn test_openai_compatible_task_endpoint_detector_respects_task_gate_and_methods() {
    assert!(detect_openai_compatible_task_endpoint(
        false,
        "POST",
        "/v1/images/generations",
        "application/json"
    )
    .is_none());
    assert!(detect_openai_compatible_task_endpoint(
        true,
        "GET",
        "/v1/images/generations",
        "application/json"
    )
    .is_none());
    assert!(detect_openai_compatible_task_endpoint(
        true,
        "POST",
        "/v1/chat/completions",
        "application/json"
    )
    .is_none());
}

#[test]
fn test_non_task_request_passes_through() {
    assert_eq!(detect_task_shape(false, "POST", "/v1/tasks", None), None);
    assert_eq!(detect_task_shape(true, "GET", "/v1/tasks", None), None);
    assert_eq!(
        detect_task_shape(true, "POST", "/v1/chat/completions", None),
        None
    );
    assert_eq!(
        detect_task_shape(true, "POST", "/v1/tasks/task_123/watch", None),
        None
    );

    let ctx = ReqCtx::default();
    assert!(!ctx.is_task);
    assert!(ctx.task_action.is_none());
    assert!(ctx.task_endpoint_candidate.is_none());
}

#[test]
fn test_task_request_filter_sets_context() {
    let mut ctx = ReqCtx::default();

    assert!(mark_task_context(
        &mut ctx,
        true,
        "POST",
        "/v1/tasks/task_123/cancel",
        None
    ));

    assert!(ctx.is_task);
    assert_eq!(ctx.task_action, Some(TaskAction::Cancel));
    assert!(ctx.task_endpoint_candidate.is_none());
}

#[test]
fn test_openai_compatible_task_request_filter_sets_candidate_context_without_legacy_action() {
    let mut ctx = ReqCtx::default();

    assert!(mark_task_context(
        &mut ctx,
        true,
        "POST",
        "/v1/embeddings",
        Some("application/json"),
    ));

    assert!(ctx.is_task);
    assert!(ctx.task_action.is_none());
    let candidate = ctx
        .task_endpoint_candidate
        .as_ref()
        .expect("task endpoint candidate");
    assert_eq!(candidate.task_kind, "embedding");
    assert_eq!(candidate.model_path.as_deref(), Some("$.model"));
}

#[test]
fn test_openai_compatible_task_request_key_extracts_model_and_task_kind() {
    let cases = [
        (
            "POST",
            "/v1/images/generations",
            serde_json::json!({"model": "gpt-image-2", "prompt": "x"}),
            "gpt-image-2",
            "image",
        ),
        (
            "POST",
            "/v1/embeddings",
            serde_json::json!({"model": "text-embedding-3-large", "input": "x"}),
            "text-embedding-3-large",
            "embedding",
        ),
        (
            "POST",
            "/v1/videos",
            serde_json::json!({"model": "sora-2", "prompt": "x"}),
            "sora-2",
            "video",
        ),
    ];

    for (method, path, body, model, task_kind) in cases {
        let candidate =
            detect_openai_compatible_task_endpoint(true, method, path, "application/json")
                .expect("candidate");
        let key = resolve_task_request_key(&candidate, &body).expect("request key");

        assert_eq!(key.global_model_id, model);
        assert_eq!(key.task_kind, task_kind);
        assert_eq!(key.cache_key(), format!("{model}:{task_kind}"));
    }
}

#[test]
fn test_openai_compatible_task_request_key_requires_model_for_submit_endpoints() {
    let candidate = detect_openai_compatible_task_endpoint(
        true,
        "POST",
        "/v1/images/generations",
        "application/json",
    )
    .expect("candidate");

    let err = resolve_task_request_key(&candidate, &serde_json::json!({"prompt": "x"}))
        .expect_err("missing model");

    assert_eq!(err.status, 400);
    assert_eq!(err.code, "GW_TASK_MODEL_REQUIRED");
}

#[test]
fn test_openai_compatible_task_request_key_uses_endpoint_default_model() {
    let candidate = TaskEndpointCandidate {
        method: "POST".to_string(),
        path: "/v1/videos".to_string(),
        task_kind: "video".to_string(),
        model_path: Some("$.model".to_string()),
        default_model: Some("sora-2".to_string()),
        content_type: Some("application/json".to_string()),
    };

    let key = resolve_task_request_key(&candidate, &serde_json::json!({"prompt": "orbit"}))
        .expect("request key");

    assert_eq!(key.global_model_id, "sora-2");
    assert_eq!(key.task_kind, "video");
    assert_eq!(key.cache_key(), "sora-2:video");
}

#[tokio::test]
async fn test_openai_compatible_multipart_request_extracts_model_and_file_parts() {
    let candidate = detect_openai_compatible_task_endpoint(
        true,
        "POST",
        "/v1/images/edits",
        "multipart/form-data; boundary=X-BOUNDARY",
    )
    .expect("candidate");
    let body = concat!(
        "--X-BOUNDARY\r\n",
        "Content-Disposition: form-data; name=\"model\"\r\n\r\n",
        "gpt-image-2\r\n",
        "--X-BOUNDARY\r\n",
        "Content-Disposition: form-data; name=\"prompt\"\r\n\r\n",
        "make it brighter\r\n",
        "--X-BOUNDARY\r\n",
        "Content-Disposition: form-data; name=\"image\"; filename=\"input.png\"\r\n",
        "Content-Type: image/png\r\n\r\n",
        "PNGDATA\r\n",
        "--X-BOUNDARY--\r\n"
    );

    let client_request = parse_openai_compatible_client_request(
        &candidate,
        Some("multipart/form-data; boundary=X-BOUNDARY"),
        body.as_bytes(),
    )
    .await
    .expect("multipart request");
    let key = resolve_task_request_key(&candidate, &client_request).expect("request key");

    assert_eq!(key.global_model_id, "gpt-image-2");
    assert_eq!(client_request["prompt"], "make it brighter");
    assert_eq!(client_request["image"]["filename"], "input.png");
    assert_eq!(client_request["image"]["content_type"], "image/png");
    assert_eq!(client_request["image"]["bytes_b64"], "UE5HREFUQQ==");
    assert_eq!(
        client_request["_multipart"]["fields"]["model"],
        "gpt-image-2"
    );
    assert_eq!(
        client_request["_multipart"]["files"]["image"][0]["filename"],
        "input.png"
    );
}
