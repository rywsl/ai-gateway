use crate::ctx::ReqCtx;
use bytes::Bytes;
use gw_billing::formulas::{calculate_freeze_amount, TaskBillingConfig};
use gw_core::error::codes;
use gw_core::ids::UserId;
use gw_store::deduction::{DeductionState, DEDUCT};
use gw_task::{CancelReason, TaskContract, TaskError, TaskId, TaskKind, TaskRecord, TaskStatus};
use pingora::http::ResponseHeader;
use pingora::Result;
use pingora_proxy::Session;
use serde_json::json;
use std::borrow::Cow;
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAction {
    Submit,
    Load,
    Watch,
    Cancel,
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
        ("POST", ["v1", "images", "generations"]) => Some(TaskAction::Submit),
        ("POST", ["v1", "responses"]) if body_has_background_true(_body) => {
            Some(TaskAction::Submit)
        }
        _ => None,
    }
}

fn body_has_background_true(body: Option<&[u8]>) -> bool {
    let Some(body) = body else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    value
        .get("background")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
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
    let input_payload: serde_json::Value = serde_json::from_slice(request.body)
        .map_err(|err| TaskHttpError::submit_failed(format!("invalid task payload: {err}")))?;
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
    if frozen_micro > 0 {
        try_freeze_task(
            request.user_id,
            frozen_micro,
            request.available_balance_micro,
        )?;
        metrics::counter!("gw_task_gw_freeze_total", "result" => "ok").increment(1);
    }
    let record = TaskRecord {
        id: TaskId::new_v4(),
        schema_version: 1,
        kind,
        status: TaskStatus::Queued,
        user_id: request.user_id.to_string(),
        child_id: None,
        api_key_id: request.api_key_id.unwrap_or_default().to_string(),
        client_protocol: request.client_protocol.to_string(),
        vendor_id: input_payload
            .get("vendor_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        vendor_key_hint: input_payload
            .get("vendor_key_hint")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        model: input_payload
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
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
                release_local_task_freeze(request.user_id, frozen_micro);
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

fn try_freeze_task(
    user_id: &str,
    amount: i64,
    available_balance_micro: Option<i64>,
) -> std::result::Result<(), TaskHttpError> {
    if available_balance_micro.is_some_and(|balance| balance < amount) {
        metrics::counter!("gw_task_gw_freeze_total", "result" => "insufficient").increment(1);
        return Err(TaskHttpError::freeze_failed("insufficient task balance"));
    }
    let uid = UserId::from(user_id);
    let state = DEDUCT
        .entry(uid)
        .or_insert_with(|| {
            Arc::new(DeductionState {
                base_micro: AtomicI64::new(available_balance_micro.unwrap_or(0)),
                deducted_micro: AtomicI64::new(0),
                frozen_micro: AtomicI64::new(0),
            })
        })
        .clone();
    loop {
        let frozen = state.frozen_micro.load(Ordering::Acquire);
        let real = state.base_micro.load(Ordering::Acquire)
            - state.deducted_micro.load(Ordering::Acquire)
            - frozen;
        if real < amount {
            metrics::counter!("gw_task_gw_freeze_total", "result" => "insufficient").increment(1);
            return Err(TaskHttpError::freeze_failed("insufficient task balance"));
        }
        if state
            .frozen_micro
            .compare_exchange(frozen, frozen + amount, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return Ok(());
        }
    }
}

fn release_local_task_freeze(user_id: &str, amount: i64) {
    if let Some(state) = DEDUCT
        .get(&UserId::from(user_id))
        .map(|entry| entry.clone())
    {
        state.frozen_micro.fetch_sub(amount, Ordering::AcqRel);
    }
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

fn now_ms() -> u64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as u64
}
