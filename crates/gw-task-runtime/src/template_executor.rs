use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gw_task::template::{
    eval_json_path, hash_effective_template, render, Context, RenderLimits, TemplateError,
};
use gw_task::{ClientMode, TaskKind, UpstreamMode};
use serde_json::{Map, Value};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionIdentity {
    pub user_id: String,
    pub api_key_id: String,
    pub request_id: String,
    pub trace_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionRequest {
    pub identity: ExecutionIdentity,
    pub task_kind: TaskKind,
    pub client_mode: ClientMode,
    pub upstream_mode: UpstreamMode,
    pub profile: Value,
    pub binding: Value,
    pub client_request: Value,
    pub task: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpstreamRequest {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UpstreamResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PollContext {
    pub provider_task_id: Option<String>,
    pub poll_url: Option<String>,
    pub submit_response: Value,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TransportError {
    #[error("TransportNotWired")]
    NotWired,
    #[error("upstream transport failed: {0}")]
    Failed(String),
}

#[async_trait]
pub trait UpstreamTransport: Send + Sync {
    async fn submit(&self, request: UpstreamRequest) -> Result<UpstreamResponse, TransportError>;
    async fn poll(&self, ctx: PollContext) -> Result<UpstreamResponse, TransportError>;
}

#[derive(Clone, Debug, Default)]
pub struct NotWiredTransport;

#[async_trait]
impl UpstreamTransport for NotWiredTransport {
    async fn submit(&self, _request: UpstreamRequest) -> Result<UpstreamResponse, TransportError> {
        Err(TransportError::NotWired)
    }

    async fn poll(&self, _ctx: PollContext) -> Result<UpstreamResponse, TransportError> {
        Err(TransportError::NotWired)
    }
}

#[derive(Clone)]
pub struct TemplateExecutor {
    transport: Arc<dyn UpstreamTransport>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionResult {
    Completed {
        response: Value,
        events: Vec<ExecutionEvent>,
    },
    Submitted {
        response: Value,
        events: Vec<ExecutionEvent>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionEvent {
    pub status: String,
}

impl TemplateExecutor {
    #[must_use]
    pub fn new(transport: Arc<dyn UpstreamTransport>) -> Self {
        Self { transport }
    }

    pub async fn execute_inline(
        &self,
        request: ExecutionRequest,
    ) -> Result<ExecutionResult, TemplateError> {
        self.execute(request, true).await
    }

    pub async fn execute_background(
        &self,
        request: ExecutionRequest,
    ) -> Result<ExecutionResult, TemplateError> {
        self.execute(request, false).await
    }

    async fn execute(
        &self,
        request: ExecutionRequest,
        inline: bool,
    ) -> Result<ExecutionResult, TemplateError> {
        let mut events = vec![
            ExecutionEvent {
                status: "submitted".to_string(),
            },
            ExecutionEvent {
                status: "running".to_string(),
            },
        ];
        let rendered = render_upstream_request(&request)?;
        let upstream_response = self
            .transport
            .submit(rendered)
            .await
            .map_err(|err| TemplateError::RenderFailed(err.to_string()))?;
        let final_response = if request.upstream_mode == UpstreamMode::AsyncLro
            && provider_task_pending(&upstream_response.body)
        {
            let poll = self
                .transport
                .poll(PollContext {
                    provider_task_id: provider_task_id(&upstream_response.body),
                    poll_url: poll_url(&upstream_response.body),
                    submit_response: upstream_response.body.clone(),
                })
                .await
                .map_err(|err| TemplateError::RenderFailed(err.to_string()))?;
            poll.body
        } else {
            upstream_response.body
        };

        let client_final = render_client_response(&request, &final_response)?;
        events.push(ExecutionEvent {
            status: "succeeded".to_string(),
        });

        if inline || request.client_mode == ClientMode::Sync {
            Ok(ExecutionResult::Completed {
                response: client_final,
                events,
            })
        } else {
            Ok(ExecutionResult::Submitted {
                response: client_final,
                events,
            })
        }
    }
}

fn render_upstream_request(request: &ExecutionRequest) -> Result<UpstreamRequest, TemplateError> {
    let client = normalize_client(
        request
            .profile
            .get("client_request_template")
            .ok_or_else(|| {
                TemplateError::Invalid("client_request_template is required".to_string())
            })?,
        &request.client_request,
    )?;
    let ctx = context_for(request, Value::Object(client), Value::Null);
    let tmpl = request
        .profile
        .get("upstream_request_template")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            TemplateError::Invalid("upstream_request_template is required".to_string())
        })?;
    let method = tmpl
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("POST")
        .to_string();
    let path = tmpl
        .get("path")
        .map(|value| render(&ctx, value, RenderLimits::default()))
        .transpose()?
        .map(value_as_string)
        .unwrap_or_default();
    let headers_value = tmpl
        .get("headers")
        .map(|value| render(&ctx, value, RenderLimits::default()))
        .transpose()?
        .unwrap_or_else(|| Value::Object(Map::new()));
    let headers = headers_value
        .as_object()
        .map(|headers| {
            headers
                .iter()
                .map(|(key, value)| (key.clone(), value_as_string(value.clone())))
                .collect()
        })
        .unwrap_or_default();
    let body = tmpl
        .get("body")
        .map(|value| render(&ctx, value, RenderLimits::default()))
        .transpose()?
        .unwrap_or(Value::Null);

    Ok(UpstreamRequest {
        method,
        path,
        headers,
        body,
    })
}

fn render_client_response(
    request: &ExecutionRequest,
    upstream_response: &Value,
) -> Result<Value, TemplateError> {
    let client = normalize_client(
        request
            .profile
            .get("client_request_template")
            .ok_or_else(|| {
                TemplateError::Invalid("client_request_template is required".to_string())
            })?,
        &request.client_request,
    )?;
    let result = render_result(
        request
            .profile
            .get("upstream_response_template")
            .and_then(|value| value.get("result")),
        upstream_response,
    )?;
    let ctx = context_for(request, Value::Object(client), result);
    let tmpl = request
        .profile
        .get("client_sync_response_template")
        .ok_or_else(|| {
            TemplateError::Invalid("client_sync_response_template is required".to_string())
        })?;
    render(&ctx, tmpl, RenderLimits::default())
}

fn context_for(request: &ExecutionRequest, client: Value, result: Value) -> Context {
    let mut ctx = Context::new();
    ctx.insert("binding".to_string(), request.binding.clone());
    ctx.insert("client".to_string(), client);
    ctx.insert("task".to_string(), request.task.clone());
    if !result.is_null() {
        ctx.insert("result".to_string(), result);
    }
    ctx
}

fn normalize_client(spec: &Value, req: &Value) -> Result<Map<String, Value>, TemplateError> {
    let mut out = Map::new();
    let Some(normalize) = spec.get("normalize").and_then(Value::as_object) else {
        return Ok(out);
    };
    for (target, source) in normalize {
        let value = if let Some(path) = source.as_str() {
            eval_json_path(req, path)?
        } else if let Some(obj) = source.as_object() {
            let path = obj.get("value").and_then(Value::as_str).ok_or_else(|| {
                TemplateError::RenderFailed("normalize value path is required".to_string())
            })?;
            match eval_json_path(req, path) {
                Ok(value) => value,
                Err(err) => obj.get("default").cloned().ok_or(err)?,
            }
        } else {
            source.clone()
        };
        set_dotted(
            &mut out,
            target.strip_prefix("client.").unwrap_or(target),
            value,
        );
    }
    Ok(out)
}

fn render_result(spec: Option<&Value>, upstream: &Value) -> Result<Value, TemplateError> {
    let mut out = Map::new();
    let Some(spec) = spec.and_then(Value::as_object) else {
        return Ok(Value::Object(out));
    };
    for (target, path) in spec {
        let Some(path) = path.as_str() else {
            continue;
        };
        let value = eval_json_path(upstream, path)?;
        set_dotted(&mut out, target, value);
    }
    Ok(Value::Object(out))
}

fn set_dotted(out: &mut Map<String, Value>, path: &str, value: Value) {
    let mut parts = path.split('.').peekable();
    let mut cur = out;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            cur.insert(part.to_string(), value);
            return;
        }
        let entry = cur
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        cur = entry.as_object_mut().expect("object inserted");
    }
}

fn provider_task_pending(value: &Value) -> bool {
    value
        .get("status")
        .or_else(|| value.get("state"))
        .and_then(Value::as_str)
        .map(|status| matches!(status, "queued" | "pending" | "running" | "processing"))
        .unwrap_or(false)
}

fn provider_task_id(value: &Value) -> Option<String> {
    value
        .get("provider_task_id")
        .or_else(|| value.get("id"))
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn poll_url(value: &Value) -> Option<String> {
    value
        .get("status_url")
        .or_else(|| value.get("poll_url"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn value_as_string(value: Value) -> String {
    match value {
        Value::String(value) => value,
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[allow(dead_code)]
fn effective_template_hash(profile: &Value, binding: &Value) -> String {
    hash_effective_template(&serde_json::json!({
        "profile": profile,
        "binding": binding,
    }))
}
