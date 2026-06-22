use gw_proxy::ctx::ReqCtx;
use gw_proxy::filters::task::{
    detect_published_task_endpoint, detect_task_shape, mark_task_context, EndpointRegistryEntry,
    TaskAction,
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
}
