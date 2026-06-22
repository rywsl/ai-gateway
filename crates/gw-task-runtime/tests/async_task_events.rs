use gw_task::TaskId;
use gw_task_runtime::events::{async_task_event_fields, AsyncTaskEvent, ASYNC_TASK_EVENTS_STREAM};
use serde_json::json;

#[test]
fn async_task_event_fields_include_projection_contract() {
    let task_id = TaskId::new_v4();
    let event = AsyncTaskEvent {
        event_id: "evt_1".to_string(),
        task_id,
        event_seq: 3,
        record_version: 4,
        payload_schema_version: 1,
        status: "succeeded".to_string(),
        user_id: "user_1".to_string(),
        api_key_id: "ak_1".to_string(),
        task_kind: "image".to_string(),
        client_mode: "sync".to_string(),
        upstream_mode: "sync".to_string(),
        global_model_id: "gpt-image".to_string(),
        provider_model_id: "gpt-image-2".to_string(),
        endpoint_id: "endpoint_1".to_string(),
        provider_key_id: Some("key_1".to_string()),
        profile_id: "profile_1".to_string(),
        profile_revision_id: "profile_1:v1".to_string(),
        profile_version: 1,
        binding_id: "binding_1".to_string(),
        binding_revision_id: "binding_1:v1".to_string(),
        binding_version: 1,
        effective_template_hash: "sha256:test".to_string(),
        request_json: json!({"prompt": "x"}),
        response_json: Some(json!({"data": []})),
        mapping_error: None,
        created_at_ms: 1_700_000_000_000,
    };

    let fields = async_task_event_fields(&event).expect("event fields");

    assert_eq!(ASYNC_TASK_EVENTS_STREAM, "async_tasks:events");
    assert_eq!(fields.get("event_id"), Some(&"evt_1".to_string()));
    assert_eq!(fields.get("task_id"), Some(&task_id.to_string()));
    assert_eq!(fields.get("event_seq"), Some(&"3".to_string()));
    assert_eq!(fields.get("record_version"), Some(&"4".to_string()));
    assert_eq!(fields.get("payload_schema_version"), Some(&"1".to_string()));
    assert_eq!(
        fields.get("effective_template_hash"),
        Some(&"sha256:test".to_string())
    );
    assert_eq!(
        fields.get("request_json"),
        Some(&"{\"prompt\":\"x\"}".to_string())
    );
    assert_eq!(
        fields.get("response_json"),
        Some(&"{\"data\":[]}".to_string())
    );
    assert!(fields.contains_key("created_at_ms"));
}
