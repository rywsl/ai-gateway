use gw_proxy::ctx::ReqCtx;
use gw_proxy::filters::task::{detect_task_shape, mark_task_context, TaskAction};

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
    assert_eq!(
        detect_task_shape(
            true,
            "POST",
            "/v1/responses",
            Some(br#"{"model":"gpt-4.1","background":true}"#)
        ),
        Some(TaskAction::Submit)
    );
    assert_eq!(
        detect_task_shape(true, "POST", "/v1/images/generations", Some(br#"{}"#)),
        Some(TaskAction::Submit)
    );
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
