use std::str::FromStr;

use gw_task::{ClientMode, TaskId, TaskKind, TaskRecord, TaskStatus, UpstreamMode};
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
    assert!(TaskStatus::Running.can_transition_to(TaskStatus::TimedOut));
    assert!(TaskStatus::Cancelling.can_transition_to(TaskStatus::TimedOut));

    assert!(!TaskStatus::Pending.can_transition_to(TaskStatus::Running));
    assert!(!TaskStatus::Queued.can_transition_to(TaskStatus::Succeeded));
    assert!(!TaskStatus::Succeeded.can_transition_to(TaskStatus::Running));
    assert!(!TaskStatus::Failed.can_transition_to(TaskStatus::Queued));
    assert!(!TaskStatus::Cancelled.can_transition_to(TaskStatus::Queued));
    assert!(TaskStatus::TimedOut.is_terminal());
    assert!(!TaskStatus::TimedOut.can_transition_to(TaskStatus::Queued));
}

#[test]
fn test_task_record_serialize() {
    let record = TaskRecord {
        id: TaskId::new_v4(),
        schema_version: 2,
        kind: TaskKind::Video,
        status: TaskStatus::Queued,
        user_id: "user-1".to_owned(),
        child_id: Some("child-1".to_owned()),
        api_key_id: "key-1".to_owned(),
        vendor_id: "vendor-1".to_owned(),
        model: Some("video-model".to_owned()),
        client_mode: ClientMode::Sync,
        upstream_mode: UpstreamMode::AsyncLro,
        global_model_id: Some("global-video".to_owned()),
        provider_model_id: Some("provider-video".to_owned()),
        endpoint_id: Some("endpoint-1".to_owned()),
        provider_key_id: Some("vendor-key-1".to_owned()),
        profile_id: Some("profile-1".to_owned()),
        profile_revision_id: Some("profile-rev-1".to_owned()),
        profile_version: Some(3),
        binding_id: Some("binding-1".to_owned()),
        binding_revision_id: Some("binding-rev-1".to_owned()),
        binding_version: Some(5),
        effective_template_hash: Some("sha256:test".to_owned()),
        poll_context: Some(json!({"operation": "op-1"})),
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
fn test_task_record_v2_metadata_is_required_for_template_tasks() {
    let record = TaskRecord {
        id: TaskId::new_v4(),
        schema_version: 2,
        kind: TaskKind::Embedding,
        status: TaskStatus::Queued,
        user_id: "user-1".to_owned(),
        child_id: None,
        api_key_id: "key-1".to_owned(),
        vendor_id: "vendor-1".to_owned(),
        model: Some("embedding-model".to_owned()),
        client_mode: ClientMode::Sync,
        upstream_mode: UpstreamMode::AsyncLro,
        global_model_id: Some("text-embedding".to_owned()),
        provider_model_id: Some("provider-embedding".to_owned()),
        endpoint_id: Some("endpoint-1".to_owned()),
        provider_key_id: Some("vendor-key-1".to_owned()),
        profile_id: Some("profile-1".to_owned()),
        profile_revision_id: Some("profile-rev-1".to_owned()),
        profile_version: Some(7),
        binding_id: Some("binding-1".to_owned()),
        binding_revision_id: Some("binding-rev-1".to_owned()),
        binding_version: Some(11),
        effective_template_hash: Some("sha256:test".to_owned()),
        poll_context: Some(json!({"poll_url": "/operations/op-1"})),
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
        input_payload: json!({"input": "embed this"}),
        output_payload: None,
        error_message: None,
        frozen_micro: 50_000,
        actual_micro: None,
        duration_seconds: None,
        billing_record_id: None,
    };

    assert_eq!(record.schema_version, 2);
    assert_eq!(record.kind, TaskKind::Embedding);
    assert_eq!(record.client_mode, ClientMode::Sync);
    assert_eq!(record.upstream_mode, UpstreamMode::AsyncLro);
    assert_eq!(
        record.effective_template_hash.as_deref(),
        Some("sha256:test")
    );
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
        TaskKind::Embedding,
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
            "generic",
            "embedding"
        ])
    );

    let decoded: Vec<TaskKind> = serde_json::from_value(encoded).expect("task kinds deserialize");
    assert_eq!(decoded, kinds);
}
