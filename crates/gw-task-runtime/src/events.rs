use std::collections::HashMap;

use fred::clients::RedisPool;
use fred::interfaces::StreamsInterface;
use gw_task::TaskId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ASYNC_TASK_EVENTS_STREAM: &str = "async_tasks:events";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AsyncTaskEvent {
    pub event_id: String,
    pub task_id: TaskId,
    pub event_seq: u64,
    pub record_version: u64,
    pub payload_schema_version: u64,
    pub status: String,
    pub user_id: String,
    pub api_key_id: String,
    pub task_kind: String,
    pub client_mode: String,
    pub upstream_mode: String,
    pub global_model_id: String,
    pub provider_model_id: String,
    pub endpoint_id: String,
    pub provider_key_id: Option<String>,
    pub profile_id: String,
    pub profile_revision_id: String,
    pub profile_version: u64,
    pub binding_id: String,
    pub binding_revision_id: String,
    pub binding_version: u64,
    pub effective_template_hash: String,
    pub request_json: Value,
    pub response_json: Option<Value>,
    pub mapping_error: Option<String>,
    pub created_at_ms: u64,
}

pub fn async_task_event_fields(
    event: &AsyncTaskEvent,
) -> Result<HashMap<String, String>, serde_json::Error> {
    let mut fields = HashMap::new();
    fields.insert("event_id".to_string(), event.event_id.clone());
    fields.insert("task_id".to_string(), event.task_id.to_string());
    fields.insert("event_seq".to_string(), event.event_seq.to_string());
    fields.insert(
        "record_version".to_string(),
        event.record_version.to_string(),
    );
    fields.insert(
        "payload_schema_version".to_string(),
        event.payload_schema_version.to_string(),
    );
    fields.insert("status".to_string(), event.status.clone());
    fields.insert("user_id".to_string(), event.user_id.clone());
    fields.insert("api_key_id".to_string(), event.api_key_id.clone());
    fields.insert("task_kind".to_string(), event.task_kind.clone());
    fields.insert("client_mode".to_string(), event.client_mode.clone());
    fields.insert("upstream_mode".to_string(), event.upstream_mode.clone());
    fields.insert("global_model_id".to_string(), event.global_model_id.clone());
    fields.insert(
        "provider_model_id".to_string(),
        event.provider_model_id.clone(),
    );
    fields.insert("endpoint_id".to_string(), event.endpoint_id.clone());
    if let Some(provider_key_id) = &event.provider_key_id {
        fields.insert("provider_key_id".to_string(), provider_key_id.clone());
    }
    fields.insert("profile_id".to_string(), event.profile_id.clone());
    fields.insert(
        "profile_revision_id".to_string(),
        event.profile_revision_id.clone(),
    );
    fields.insert(
        "profile_version".to_string(),
        event.profile_version.to_string(),
    );
    fields.insert("binding_id".to_string(), event.binding_id.clone());
    fields.insert(
        "binding_revision_id".to_string(),
        event.binding_revision_id.clone(),
    );
    fields.insert(
        "binding_version".to_string(),
        event.binding_version.to_string(),
    );
    fields.insert(
        "effective_template_hash".to_string(),
        event.effective_template_hash.clone(),
    );
    fields.insert(
        "request_json".to_string(),
        serde_json::to_string(&event.request_json)?,
    );
    if let Some(response_json) = &event.response_json {
        fields.insert(
            "response_json".to_string(),
            serde_json::to_string(response_json)?,
        );
    }
    if let Some(mapping_error) = &event.mapping_error {
        fields.insert("mapping_error".to_string(), mapping_error.clone());
    }
    fields.insert("created_at_ms".to_string(), event.created_at_ms.to_string());
    Ok(fields)
}

pub async fn write_async_task_event(
    redis: &RedisPool,
    event: AsyncTaskEvent,
) -> anyhow::Result<()> {
    let fields = async_task_event_fields(&event)?;
    let _: String = redis
        .next()
        .xadd(ASYNC_TASK_EVENTS_STREAM, false, None::<()>, "*", fields)
        .await?;
    Ok(())
}
