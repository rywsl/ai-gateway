use std::sync::Arc;

use bytes::Bytes;
use gw_task::{ClientMode, TaskKind, UpstreamMode};
use gw_task_runtime::http_transport::HttpTemplateTransport;
use gw_task_runtime::template_executor::{
    ExecutionIdentity, ExecutionRequest, ExecutionResult, TemplateExecutor,
};
use pingora::http::ResponseHeader;
use pingora::Result;
use pingora_proxy::Session;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEBUG_TASK_RUNTIME_PATH: &str = "/_gateway/admin/debug/task-runtime";

#[derive(Debug, Deserialize)]
pub struct DebugTaskRuntimeRequest {
    pub profile: Value,
    #[serde(default)]
    pub binding: Value,
    #[serde(default)]
    pub client_request: Value,
    #[serde(default = "default_task_kind")]
    pub task_kind: TaskKind,
    #[serde(default = "default_client_mode")]
    pub client_mode: ClientMode,
    #[serde(default = "default_upstream_mode")]
    pub upstream_mode: UpstreamMode,
    #[serde(default)]
    pub upstream_base_url: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub api_key_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
    #[serde(default)]
    pub task: Option<Value>,
}

#[derive(Debug, Serialize)]
struct DebugExecutionEvent {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    poll_wait_ms: Option<u64>,
}

pub fn is_debug_task_runtime_path(method: &str, path: &str) -> bool {
    method.eq_ignore_ascii_case("POST") && path == DEBUG_TASK_RUNTIME_PATH
}

pub async fn handle_task_runtime_debug(
    session: &mut Session,
    body: &[u8],
) -> std::result::Result<(), DebugTaskHttpError> {
    let req: DebugTaskRuntimeRequest = serde_json::from_slice(body).map_err(|err| {
        DebugTaskHttpError::bad_request(format!("invalid debug task runtime json: {err}"))
    })?;
    let base_url = req.upstream_base_url.clone();
    let transport = HttpTemplateTransport::with_default_timeout(base_url)
        .map_err(|err| DebugTaskHttpError::internal(format!("build task transport: {err}")))?;
    let executor = TemplateExecutor::new(Arc::new(transport));
    let request_id = req
        .request_id
        .unwrap_or_else(|| "debug_task_runtime".to_string());
    let execution = ExecutionRequest {
        identity: ExecutionIdentity {
            user_id: req.user_id.unwrap_or_else(|| "debug_admin".to_string()),
            api_key_id: req.api_key_id.unwrap_or_default(),
            trace_id: req.trace_id.unwrap_or_else(|| request_id.clone()),
            request_id: request_id.clone(),
        },
        task_kind: req.task_kind,
        client_mode: req.client_mode,
        upstream_mode: req.upstream_mode,
        profile: req.profile,
        binding: req.binding,
        client_request: req.client_request,
        task: req.task.unwrap_or_else(|| {
            json!({
                "id": request_id,
                "status": "queued",
            })
        }),
    };

    let result = executor.execute_inline(execution).await.map_err(|err| {
        DebugTaskHttpError::internal(format!("execute debug task runtime: {}: {err}", err.code()))
    })?;
    let body = match result {
        ExecutionResult::Completed { response, events } => json!({
            "status": "completed",
            "client_response": response,
            "events": debug_events(events),
        }),
        ExecutionResult::Submitted { response, events } => json!({
            "status": "submitted",
            "client_response": response,
            "events": debug_events(events),
        }),
    };
    write_json(session, 200, body)
        .await
        .map_err(|err| DebugTaskHttpError::internal(format!("write debug response: {err}")))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugTaskHttpError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl DebugTaskHttpError {
    pub(crate) fn bad_request(message: String) -> Self {
        Self {
            status: 400,
            code: "GW_TASK_DEBUG_INVALID_REQUEST",
            message,
        }
    }

    pub(crate) fn internal(message: String) -> Self {
        Self {
            status: 500,
            code: "GW_TASK_DEBUG_EXECUTION_FAILED",
            message,
        }
    }

    pub async fn write(self, session: &mut Session) -> Result<()> {
        write_json(
            session,
            self.status,
            json!({
                "error": {
                    "code": self.code,
                    "message": self.message,
                }
            }),
        )
        .await
    }
}

async fn write_json(session: &mut Session, status: u16, body: Value) -> Result<()> {
    let body = Bytes::from(body.to_string());
    let mut header = ResponseHeader::build(status, None)?;
    header.insert_header("Content-Type", "application/json")?;
    header.insert_header("Cache-Control", "no-cache")?;
    header.insert_header("Content-Length", body.len().to_string())?;
    session
        .write_response_header(Box::new(header), false)
        .await?;
    session.write_response_body(Some(body), true).await
}

fn debug_events(
    events: Vec<gw_task_runtime::template_executor::ExecutionEvent>,
) -> Vec<DebugExecutionEvent> {
    events
        .into_iter()
        .map(|event| DebugExecutionEvent {
            status: event.status,
            poll_wait_ms: event.poll_wait_ms,
        })
        .collect()
}

fn default_task_kind() -> TaskKind {
    TaskKind::Generic
}

fn default_client_mode() -> ClientMode {
    ClientMode::Sync
}

fn default_upstream_mode() -> UpstreamMode {
    UpstreamMode::Sync
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_only_gateway_internal_debug_task_runtime_route() {
        assert!(is_debug_task_runtime_path(
            "POST",
            "/_gateway/admin/debug/task-runtime"
        ));
        assert!(!is_debug_task_runtime_path(
            "GET",
            "/_gateway/admin/debug/task-runtime"
        ));
        assert!(!is_debug_task_runtime_path(
            "POST",
            "/admin/debug/task-runtime"
        ));
    }

    #[test]
    fn parses_minimal_debug_request_defaults() {
        let req: DebugTaskRuntimeRequest = serde_json::from_value(json!({
            "profile": {},
            "client_request": {"model": "m"},
        }))
        .expect("request parses");
        assert_eq!(req.task_kind, TaskKind::Generic);
        assert_eq!(req.client_mode, ClientMode::Sync);
        assert_eq!(req.upstream_mode, UpstreamMode::Sync);
    }
}
