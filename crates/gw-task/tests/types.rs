use std::str::FromStr;

use gw_task::{TaskId, TaskKind, TaskRecord, TaskStatus};
use serde_json::json;

#[test]
fn test_task_id_display_roundtrip() {
    let id = TaskId::new_v4();
    let rendered = id.to_string();

    let parsed = TaskId::from_str(&rendered).expect("task id parses from display form");

    assert_eq!(parsed, id);
}

#[test]
fn test_task_status_transitions() {
    assert!(TaskStatus::Pending.can_transition_to(TaskStatus::Queued));
    assert!(TaskStatus::Queued.can_transition_to(TaskStatus::Running));
    assert!(TaskStatus::Running.can_transition_to(TaskStatus::Succeeded));
    assert!(TaskStatus::Running.can_transition_to(TaskStatus::Failed));
    assert!(TaskStatus::Running.can_transition_to(TaskStatus::Cancelled));
    assert!(TaskStatus::Running.can_transition_to(TaskStatus::Cancelling));
    assert!(TaskStatus::Cancelling.can_transition_to(TaskStatus::Cancelled));
    assert!(TaskStatus::Cancelling.can_transition_to(TaskStatus::Succeeded));

    assert!(!TaskStatus::Pending.can_transition_to(TaskStatus::Running));
    assert!(!TaskStatus::Queued.can_transition_to(TaskStatus::Succeeded));
    assert!(!TaskStatus::Succeeded.can_transition_to(TaskStatus::Running));
    assert!(!TaskStatus::Failed.can_transition_to(TaskStatus::Queued));
    assert!(!TaskStatus::Cancelled.can_transition_to(TaskStatus::Queued));
}

#[test]
fn test_task_record_serialize() {
    let record = TaskRecord {
        id: TaskId::new_v4(),
        schema_version: 1,
        kind: TaskKind::Video,
        status: TaskStatus::Queued,
        user_id: "user-1".to_owned(),
        child_id: Some("child-1".to_owned()),
        api_key_id: "key-1".to_owned(),
        client_protocol: "openai".to_owned(),
        vendor_id: "vendor-1".to_owned(),
        vendor_key_hint: Some("vendor-key-1".to_owned()),
        model: Some("video-model".to_owned()),
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_000_100,
        submitted_ts_ms: 1_700_000_000_000,
        expires_at_ms: 1_700_086_400_000,
        claimed_ts_ms: None,
        ended_ts_ms: None,
        lease_until_ms: None,
        worker_id: None,
        trace_id: Some("trace-1".to_owned()),
        request_id: Some("request-1".to_owned()),
        provider_task_id: None,
        provider_first_response_ms: None,
        input_payload: json!({"prompt": "make a video"}),
        output_payload: None,
        error_message: None,
        frozen_micro: 50_000,
        actual_micro: None,
        duration_seconds: None,
        billing_record_id: None,
    };

    let encoded = serde_json::to_string(&record).expect("record serializes");
    let decoded: TaskRecord = serde_json::from_str(&encoded).expect("record deserializes");

    assert_eq!(decoded, record);
}

#[test]
fn test_ir_wire_body_helper_reads_optional_string() {
    let payload = json!({
        "body": {"model": "gpt-4.1-mini"},
        "ir_wire_format": "openai:chat",
        "ir_wire_content_type": "application/json",
        "ir_wire_body": "{\"model\":\"gpt-4.1-mini\"}"
    });

    assert_eq!(
        gw_task::task_ir_wire_body(&payload),
        Ok(Some("{\"model\":\"gpt-4.1-mini\"}"))
    );
    assert_eq!(gw_task::task_ir_wire_format(&payload), Some("openai:chat"));
    assert_eq!(
        gw_task::task_ir_wire_content_type(&payload),
        Some("application/json")
    );
}

#[test]
fn test_ir_wire_body_helper_treats_absence_as_legacy_payload() {
    let payload = json!({"body": {"prompt": "legacy"}});

    assert_eq!(gw_task::task_ir_wire_body(&payload), Ok(None));
    assert_eq!(gw_task::task_ir_wire_format(&payload), None);
    assert_eq!(gw_task::task_ir_wire_content_type(&payload), None);
}

#[test]
fn test_ir_wire_body_helper_rejects_non_string_value() {
    let payload = json!({
        "body": {"model": "gpt-4.1-mini"},
        "ir_wire_body": {"model": "gpt-4.1-mini"}
    });

    assert_eq!(
        gw_task::task_ir_wire_body(&payload),
        Err(gw_task::TaskIrWireBodyError::InvalidType)
    );
}

#[test]
fn test_task_kind_enum() {
    let kinds = [
        TaskKind::Image,
        TaskKind::Video,
        TaskKind::Audio,
        TaskKind::Batch,
        TaskKind::LroResponses,
        TaskKind::Generic,
    ];

    let encoded = serde_json::to_value(kinds).expect("task kinds serialize");
    assert_eq!(
        encoded,
        json!([
            "image",
            "video",
            "audio",
            "batch",
            "lro_responses",
            "generic"
        ])
    );

    let decoded: Vec<TaskKind> = serde_json::from_value(encoded).expect("task kinds deserialize");
    assert_eq!(decoded, kinds);
}
