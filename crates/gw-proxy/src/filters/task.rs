use crate::ctx::ReqCtx;
use bytes::Bytes;
use gw_billing::formulas::{calculate_freeze_amount, TaskBillingConfig};
use gw_core::error::codes;
use gw_task::{
    CancelReason, ClientMode, TaskContract, TaskError, TaskId, TaskKind, TaskRecord, TaskStatus,
    UpstreamMode,
};
use pingora::http::ResponseHeader;
use pingora::Result;
use pingora_proxy::Session;
use serde_json::json;
use std::borrow::Cow;
use std::str::FromStr;
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAction {
    Submit,
    Load,
    Watch,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRegistryEntry {
    pub method: String,
    pub path: String,
    pub task_kind: String,
    pub global_model_id: String,
}

pub fn detect_task_shape(
    task_enabled: bool,
    method: &str,
    path: &str,
    _body: Option<&[u8]>,
) -> Option<TaskAction> {
    if !task_enabled {
        return None;
    }

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, segments.as_slice()) {
        ("POST", ["v1", "tasks"]) => Some(TaskAction::Submit),
        ("GET", ["v1", "tasks", task_id]) if !task_id.is_empty() => Some(TaskAction::Load),
        ("GET", ["v1", "tasks", task_id, "watch"]) if !task_id.is_empty() => {
            Some(TaskAction::Watch)
        }
        ("POST", ["v1", "tasks", task_id, "cancel"]) if !task_id.is_empty() => {
            Some(TaskAction::Cancel)
        }
        _ => None,
    }
}

pub fn detect_published_task_endpoint<'a>(
    registry: &'a [EndpointRegistryEntry],
    method: &str,
    path: &str,
) -> Option<&'a EndpointRegistryEntry> {
    registry
        .iter()
        .find(|entry| entry.method == method && entry.path == path)
}

pub fn mark_task_context(
    ctx: &mut ReqCtx,
    task_enabled: bool,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> bool {
    if let Some(action) = detect_task_shape(task_enabled, method, path, body) {
        ctx.is_task = true;
        ctx.task_action = Some(action);
        true
    } else {
        false
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskHttpResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskHttpError {
    pub status: u16,
    pub code: &'static str,
    pub message: Cow<'static, str>,
}

impl TaskHttpError {
    fn not_found() -> Self {
        Self {
            status: 404,
            code: codes::GW_TASK_NOT_FOUND,
            message: Cow::Borrowed("task not found"),
        }
    }

    fn submit_failed(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: 500,
            code: codes::GW_TASK_SUBMIT_FAILED,
            message: message.into(),
        }
    }

    fn freeze_failed(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: 402,
            code: codes::GW_TASK_FREEZE_FAILED,
            message: message.into(),
        }
    }
}

impl From<TaskError> for TaskHttpError {
    fn from(value: TaskError) -> Self {
        match value {
            TaskError::NotFound(_) => Self::not_found(),
            TaskError::QuotaExceeded => {
                metrics::counter!("gw_task_gw_freeze_total", "result" => "insufficient")
                    .increment(1);
                Self::freeze_failed("insufficient task balance")
            }
            TaskError::InvalidPayload(message) => Self::submit_failed(message),
            other => Self::submit_failed(other.to_string()),
        }
    }
}

pub struct SubmitTaskRequest<'a> {
    pub body: &'a [u8],
    pub user_id: &'a str,
    pub api_key_id: Option<&'a str>,
    pub client_protocol: &'a str,
    pub client_codec: Option<&'a gw_codec::AnyCodec>,
    pub trace_id: &'a str,
    pub request_id: &'a str,
    pub billing_config: Option<TaskBillingConfig>,
    pub available_balance_micro: Option<i64>,
}

pub async fn submit_endpoint<C: TaskContract + Sync>(
    contract: &C,
    request: SubmitTaskRequest<'_>,
) -> std::result::Result<TaskHttpResponse, TaskHttpError> {
    submit_endpoint_impl(contract, request).await
}

async fn submit_endpoint_impl<C: TaskContract + Sync>(
    contract: &C,
    request: SubmitTaskRequest<'_>,
) -> std::result::Result<TaskHttpResponse, TaskHttpError> {
    let mut input_payload: serde_json::Value = serde_json::from_slice(request.body)
        .map_err(|err| TaskHttpError::submit_failed(format!("invalid task payload: {err}")))?;
    attach_ir_wire_body(
        &mut input_payload,
        request.client_protocol,
        request.client_codec,
    )?;
    let now_ms = now_ms();
    let kind = infer_task_kind(&input_payload);
    let frozen_micro = request
        .billing_config
        .as_ref()
        .map(|config| calculate_freeze_amount(kind, config))
        .unwrap_or(0);
    if frozen_micro < 0 {
        return Err(TaskHttpError::freeze_failed("task freeze amount invalid"));
    }
    let task_id = TaskId::new_v4();
    if frozen_micro > 0 {
        contract
            .freeze_task(
                request.user_id,
                &task_id,
                frozen_micro,
                request.available_balance_micro,
            )
            .await
            .map_err(TaskHttpError::from)?;
        metrics::counter!("gw_task_gw_freeze_total", "result" => "ok").increment(1);
    }
    let record = TaskRecord {
        id: task_id,
        schema_version: 2,
        kind,
        status: TaskStatus::Queued,
        user_id: request.user_id.to_string(),
        child_id: None,
        api_key_id: request.api_key_id.unwrap_or_default().to_string(),
        vendor_id: input_payload
            .get("vendor_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        model: input_payload
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        client_mode: ClientMode::Async,
        upstream_mode: UpstreamMode::AsyncLro,
        global_model_id: input_payload
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        provider_model_id: input_payload
            .get("provider_model_id")
            .or_else(|| input_payload.get("model"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        endpoint_id: input_payload
            .get("endpoint_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        provider_key_id: input_payload
            .get("provider_key_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        profile_id: input_payload
            .get("profile_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        profile_revision_id: input_payload
            .get("profile_revision_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        profile_version: input_payload
            .get("profile_version")
            .and_then(serde_json::Value::as_u64),
        binding_id: input_payload
            .get("binding_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        binding_revision_id: input_payload
            .get("binding_revision_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        binding_version: input_payload
            .get("binding_version")
            .and_then(serde_json::Value::as_u64),
        effective_template_hash: input_payload
            .get("effective_template_hash")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        poll_context: input_payload.get("poll_context").cloned(),
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
        submitted_ts_ms: now_ms,
        expires_at_ms: now_ms + 86_400_000,
        claimed_ts_ms: None,
        ended_ts_ms: None,
        lease_until_ms: None,
        worker_id: None,
        trace_id: Some(request.trace_id.to_string()),
        request_id: Some(request.request_id.to_string()),
        provider_task_id: None,
        provider_first_response_ms: None,
        input_payload,
        output_payload: None,
        error_message: None,
        frozen_micro,
        actual_micro: None,
        duration_seconds: None,
        billing_record_id: None,
    };
    let expires_at = record.expires_at_ms;
    let id = match contract.submit(record).await {
        Ok(id) => id,
        Err(err) => {
            if frozen_micro > 0 {
                let _ = contract
                    .release_task_freeze(request.user_id, &task_id, frozen_micro)
                    .await;
            }
            return Err(TaskHttpError::from(err));
        }
    };
    metrics::counter!("gw_task_gw_submit_total").increment(1);
    Ok(TaskHttpResponse {
        status: 201,
        content_type: "application/json",
        body: json!({
            "task_id": id.to_string(),
            "status": "queued",
            "expires_at": expires_at,
        }),
    })
}

pub async fn load_endpoint<C: TaskContract + Sync>(
    contract: &C,
    task_id: &str,
) -> std::result::Result<TaskHttpResponse, TaskHttpError> {
    metrics::counter!("gw_task_gw_load_total").increment(1);
    let id = parse_task_id(task_id)?;
    let record = contract.load(&id).await.map_err(TaskHttpError::from)?;
    let record = record.ok_or_else(TaskHttpError::not_found)?;
    Ok(TaskHttpResponse {
        status: 200,
        content_type: "application/json",
        body: serde_json::to_value(record)
            .map_err(|err| TaskHttpError::submit_failed(format!("serialize task: {err}")))?,
    })
}

pub async fn watch_endpoint<C: TaskContract + Sync>(
    contract: &C,
    task_id: &str,
    wait_ms: u64,
) -> std::result::Result<TaskHttpResponse, TaskHttpError> {
    metrics::counter!("gw_task_gw_watch_total").increment(1);
    let id = parse_task_id(task_id)?;
    let record = contract
        .watch(&id, wait_ms)
        .await
        .map_err(TaskHttpError::from)?;
    let record = record.ok_or_else(TaskHttpError::not_found)?;
    Ok(TaskHttpResponse {
        status: 200,
        content_type: "text/event-stream",
        body: json!({
            "event": "status",
            "data": record,
        }),
    })
}

pub async fn cancel_endpoint<C: TaskContract + Sync>(
    contract: &C,
    task_id: &str,
) -> std::result::Result<TaskHttpResponse, TaskHttpError> {
    metrics::counter!("gw_task_gw_cancel_total").increment(1);
    let id = parse_task_id(task_id)?;
    let record = contract
        .request_cancel(&id, CancelReason::Client)
        .await
        .map_err(TaskHttpError::from)?;
    Ok(TaskHttpResponse {
        status: 200,
        content_type: "application/json",
        body: serde_json::to_value(record)
            .map_err(|err| TaskHttpError::submit_failed(format!("serialize task: {err}")))?,
    })
}

pub async fn write_task_response(session: &mut Session, response: TaskHttpResponse) -> Result<()> {
    let body = match response.content_type {
        "text/event-stream" => Bytes::from(format!(
            "event: status\ndata: {}\n\n",
            response.body["data"]
        )),
        _ => Bytes::from(response.body.to_string()),
    };
    let mut header = ResponseHeader::build(response.status, None)?;
    header.insert_header("Content-Type", response.content_type)?;
    header.insert_header("Cache-Control", "no-cache")?;
    session
        .write_response_header(Box::new(header), false)
        .await?;
    session.write_response_body(Some(body), true).await?;
    Ok(())
}

fn parse_task_id(task_id: &str) -> std::result::Result<TaskId, TaskHttpError> {
    TaskId::from_str(task_id).map_err(|_| TaskHttpError::not_found())
}

fn infer_task_kind(payload: &serde_json::Value) -> TaskKind {
    payload
        .get("task_type")
        .or_else(|| payload.get("kind"))
        .and_then(serde_json::Value::as_str)
        .map(|kind| match kind {
            "image" => TaskKind::Image,
            "video" => TaskKind::Video,
            "audio" => TaskKind::Audio,
            "batch" => TaskKind::Batch,
            "lro_responses" => TaskKind::LroResponses,
            _ => TaskKind::Generic,
        })
        .unwrap_or(TaskKind::Generic)
}

fn attach_ir_wire_body(
    payload: &mut serde_json::Value,
    client_protocol: &str,
    codec: Option<&gw_codec::AnyCodec>,
) -> std::result::Result<(), TaskHttpError> {
    let Some(codec) = codec else {
        return Ok(());
    };
    let storage_protocol = ir_storage_protocol(payload, client_protocol, codec);
    if storage_protocol.is_none() {
        return Ok(());
    }
    let storage_protocol = storage_protocol.expect("checked above");
    let source = payload.get(gw_task::TASK_BODY).unwrap_or(payload);
    let source_bytes = serde_json::to_vec(source)
        .map_err(|err| TaskHttpError::submit_failed(format!("serialize task body: {err}")))?;
    let ir = codec.parse_request(&source_bytes).map_err(|err| {
        TaskHttpError::submit_failed(format!("codec parse task request: {}", err.message))
    })?;
    let ir_value = serde_json::to_value(&ir)
        .map_err(|err| TaskHttpError::submit_failed(format!("serialize task request ir: {err}")))?;
    let mut out = bytes::BytesMut::with_capacity(source_bytes.len());
    codec.render_request(&ir, &mut out).map_err(|err| {
        TaskHttpError::submit_failed(format!("codec render task request: {}", err.message))
    })?;
    let rendered = String::from_utf8(out.to_vec())
        .map_err(|err| TaskHttpError::submit_failed(format!("rendered task body utf8: {err}")))?;
    let Some(obj) = payload.as_object_mut() else {
        return Err(TaskHttpError::submit_failed(
            "task payload must be a JSON object",
        ));
    };
    obj.insert(
        gw_task::TASK_IR_REQUEST.to_string(),
        serde_json::json!({
            "protocol": storage_protocol,
            "kind": "request",
            "value": ir_value,
        }),
    );
    obj.insert(
        gw_task::TASK_IR_WIRE_FORMAT.to_string(),
        serde_json::Value::String(storage_protocol.to_string()),
    );
    obj.insert(
        gw_task::TASK_IR_WIRE_CONTENT_TYPE.to_string(),
        serde_json::Value::String("application/json".to_string()),
    );
    obj.insert(
        gw_task::TASK_IR_WIRE_BODY.to_string(),
        serde_json::Value::String(rendered),
    );
    Ok(())
}

fn ir_storage_protocol<'a>(
    payload: &serde_json::Value,
    client_protocol: &'a str,
    codec: &gw_codec::AnyCodec,
) -> Option<&'a str> {
    match client_protocol {
        "openai:cli"
            if matches!(codec, gw_codec::AnyCodec::OpenAiResponses(_))
                && payload
                    .get("background")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                && !has_openai_responses_cli_body_marker(payload) =>
        {
            Some("openai:responses")
        }
        "claude:cli" | "openai:cli" | "openai:compact" | "gemini:cli" => None,
        other => Some(other),
    }
}

fn has_openai_responses_cli_body_marker(payload: &serde_json::Value) -> bool {
    payload
        .get("metadata")
        .and_then(|metadata| metadata.get("cli"))
        .is_some()
        || payload
            .get("compact")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
}

fn now_ms() -> u64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as u64
}
