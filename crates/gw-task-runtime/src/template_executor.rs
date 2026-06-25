use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use gw_task::template::{
    eval_json_path, hash_effective_template, render, Context, RenderLimits, TemplateError,
};
use gw_task::{ClientMode, TaskKind, TaskRecord, UpstreamMode};
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
    async fn request(&self, request: UpstreamRequest) -> Result<UpstreamResponse, TransportError> {
        self.submit(request).await
    }
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
    pub poll_wait_ms: Option<u64>,
}

pub fn execution_request_from_task_record(
    record: &TaskRecord,
) -> Result<ExecutionRequest, TemplateError> {
    let profile = record
        .input_payload
        .get("profile")
        .cloned()
        .ok_or_else(|| {
            TemplateError::Invalid("task input_payload.profile is required".to_string())
        })?;
    let binding = record
        .input_payload
        .get("binding")
        .cloned()
        .ok_or_else(|| {
            TemplateError::Invalid("task input_payload.binding is required".to_string())
        })?;
    let client_request = record
        .input_payload
        .get("client_request")
        .cloned()
        .ok_or_else(|| {
            TemplateError::Invalid("task input_payload.client_request is required".to_string())
        })?;
    Ok(ExecutionRequest {
        identity: ExecutionIdentity {
            user_id: record.user_id.clone(),
            api_key_id: record.api_key_id.clone(),
            request_id: record.request_id.clone().unwrap_or_default(),
            trace_id: record.trace_id.clone().unwrap_or_default(),
        },
        task_kind: record.kind,
        client_mode: record.client_mode,
        upstream_mode: record.upstream_mode,
        profile,
        binding,
        client_request,
        task: serde_json::json!({
            "id": record.id.to_string(),
            "status": serde_json::to_value(record.status)
                .unwrap_or_else(|_| serde_json::Value::String("queued".to_string())),
            "provider_task_id": record.provider_task_id,
            "profile_revision_id": record.profile_revision_id,
            "binding_revision_id": record.binding_revision_id,
            "effective_template_hash": record.effective_template_hash,
        }),
    })
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
        if inline && request.client_mode == ClientMode::Async {
            return Ok(ExecutionResult::Submitted {
                response: render_submit_response(&request)?,
                events: vec![ExecutionEvent {
                    status: "submitted".to_string(),
                    poll_wait_ms: None,
                }],
            });
        }

        let mut events = vec![
            ExecutionEvent {
                status: "submitted".to_string(),
                poll_wait_ms: None,
            },
            ExecutionEvent {
                status: "running".to_string(),
                poll_wait_ms: None,
            },
        ];
        let rendered = render_upstream_request(&request)?;
        let upstream_response = self
            .transport
            .submit(rendered)
            .await
            .map_err(|err| TemplateError::RenderFailed(err.to_string()))?;
        let final_response = if request.upstream_mode == UpstreamMode::AsyncLro {
            self.poll_to_terminal(&request, upstream_response.body.clone(), &mut events)
                .await?
        } else {
            upstream_response.body
        };

        let client_final = render_client_response(&request, &final_response)?;
        events.push(ExecutionEvent {
            status: "succeeded".to_string(),
            poll_wait_ms: None,
        });

        Ok(ExecutionResult::Completed {
            response: client_final,
            events,
        })
    }

    async fn poll_to_terminal(
        &self,
        request: &ExecutionRequest,
        submit_response: Value,
        events: &mut Vec<ExecutionEvent>,
    ) -> Result<Value, TemplateError> {
        let Some(poll_template) = request.profile.get("upstream_poll_template") else {
            let poll = self
                .transport
                .poll(PollContext {
                    provider_task_id: provider_task_id(&submit_response),
                    poll_url: poll_url(&submit_response),
                    submit_response,
                })
                .await
                .map_err(|err| TemplateError::RenderFailed(err.to_string()))?;
            return Ok(poll.body);
        };
        if poll_template.is_null() {
            return Err(TemplateError::Invalid(
                "upstream_poll_template is required for async_lro".to_string(),
            ));
        }

        let max_attempts = poll_template
            .get("max_attempts")
            .and_then(Value::as_u64)
            .unwrap_or(64)
            .max(1);
        let mut previous_poll_response = Value::Null;
        for _ in 0..max_attempts {
            let poll_request = render_poll_request(
                request,
                poll_template,
                &submit_response,
                &previous_poll_response,
            )?;
            let poll_response = self
                .transport
                .request(poll_request)
                .await
                .map_err(|err| TemplateError::RenderFailed(err.to_string()))?;
            match classify_poll_response(poll_template, &poll_response.body)? {
                PollState::Succeeded => return Ok(poll_response.body),
                PollState::Failed => {
                    return Err(TemplateError::RenderFailed(format!(
                        "upstream task failed: {}",
                        poll_response.body
                    )))
                }
                PollState::Pending => {
                    if let Some(wait_ms) = poll_wait_ms(&poll_response.headers, poll_template) {
                        events.push(ExecutionEvent {
                            status: "poll_wait".to_string(),
                            poll_wait_ms: Some(wait_ms),
                        });
                    }
                    previous_poll_response = poll_response.body;
                }
            }
        }
        Err(TemplateError::RenderFailed(format!(
            "upstream poll did not reach terminal state after {max_attempts} attempts"
        )))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollState {
    Pending,
    Succeeded,
    Failed,
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

fn render_poll_request(
    request: &ExecutionRequest,
    poll_template: &Value,
    submit_response: &Value,
    previous_poll_response: &Value,
) -> Result<UpstreamRequest, TemplateError> {
    let client = normalize_client(
        request
            .profile
            .get("client_request_template")
            .ok_or_else(|| {
                TemplateError::Invalid("client_request_template is required".to_string())
            })?,
        &request.client_request,
    )?;
    let result = poll_result(poll_template, submit_response)?;
    let mut ctx = context_for(request, Value::Object(client), result);
    ctx.insert(
        "upstream_submit_response".to_string(),
        submit_response.clone(),
    );
    if !previous_poll_response.is_null() {
        ctx.insert(
            "upstream_poll_response".to_string(),
            previous_poll_response.clone(),
        );
    }

    let tmpl = poll_template.as_object().ok_or_else(|| {
        TemplateError::Invalid("upstream_poll_template must be an object".to_string())
    })?;
    let method = tmpl
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_string();
    let path = if let Some(path) = tmpl.get("path") {
        value_as_string(render(&ctx, path, RenderLimits::default())?)
    } else if let Some(poll_url) = tmpl.get("poll_url") {
        render_legacy_poll_url(&ctx, poll_url, submit_response, previous_poll_response)?
    } else {
        return Err(TemplateError::Invalid(
            "upstream_poll_template.path or poll_url is required".to_string(),
        ));
    };
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

fn poll_result(poll_template: &Value, submit_response: &Value) -> Result<Value, TemplateError> {
    let mut result = Map::new();
    if let Some(path) = poll_template
        .get("provider_task_id")
        .and_then(Value::as_str)
    {
        result.insert(
            "provider_task_id".to_string(),
            eval_json_path(submit_response, path)?,
        );
    } else if let Some(id) = provider_task_id(submit_response) {
        result.insert("provider_task_id".to_string(), Value::String(id));
    }
    Ok(Value::Object(result))
}

fn render_legacy_poll_url(
    ctx: &Context,
    poll_url: &Value,
    submit_response: &Value,
    previous_poll_response: &Value,
) -> Result<String, TemplateError> {
    if !poll_url.is_object() {
        return Ok(value_as_string(render(
            ctx,
            poll_url,
            RenderLimits::default(),
        )?));
    }
    let spec = poll_url.as_object().expect("checked object");
    let source = match spec
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("upstream_submit_response")
    {
        "upstream_submit_response" => submit_response,
        "upstream_poll_response" => previous_poll_response,
        other => {
            return Err(TemplateError::Invalid(format!(
                "unsupported poll_url source {other}"
            )))
        }
    };
    let path = spec
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| TemplateError::Invalid("poll_url.path is required".to_string()))?;
    let value = eval_json_path(source, path)?;
    let Some(template) = spec.get("template") else {
        return Ok(value_as_string(value));
    };
    let mut child = ctx.clone();
    child.insert("value".to_string(), value);
    Ok(value_as_string(render(
        &child,
        template,
        RenderLimits::default(),
    )?))
}

fn classify_poll_response(
    poll_template: &Value,
    response: &Value,
) -> Result<PollState, TemplateError> {
    if let Some(status) = poll_template.get("status").and_then(Value::as_object) {
        let path = status
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("$.status");
        let value = eval_json_path(response, path)?;
        if status_values_match(status.get("succeeded"), &value) {
            return Ok(PollState::Succeeded);
        }
        if status_values_match(status.get("failed"), &value) {
            return Ok(PollState::Failed);
        }
        if status_values_match(status.get("pending"), &value) {
            return Ok(PollState::Pending);
        }
        return Err(TemplateError::RenderFailed(format!(
            "unknown upstream poll status at {path}: {value}"
        )));
    }

    if let Some(path) = poll_template.get("status_path").and_then(Value::as_str) {
        let value = eval_json_path(response, path)?;
        if poll_template
            .get("success_when")
            .is_some_and(|expected| expected == &value)
        {
            return Ok(PollState::Succeeded);
        }
        if poll_template
            .get("failed_when")
            .is_some_and(|expected| expected == &value)
        {
            return Ok(PollState::Failed);
        }
        if poll_template
            .get("pending_when")
            .is_some_and(|expected| expected == &value)
        {
            return Ok(PollState::Pending);
        }
        return Err(TemplateError::RenderFailed(format!(
            "unknown upstream poll status at {path}: {value}"
        )));
    }

    default_poll_state(response).ok_or_else(|| {
        TemplateError::RenderFailed(format!("unknown upstream poll status: {response}"))
    })
}

fn poll_wait_ms(headers: &BTreeMap<String, String>, poll_template: &Value) -> Option<u64> {
    let header_name = poll_template
        .get("next_poll_after_header")
        .and_then(Value::as_str)
        .unwrap_or("openai-poll-after-ms");
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(header_name))
        .and_then(|(_, value)| value.parse::<u64>().ok())
        .or_else(|| {
            poll_template
                .get("poll_interval_ms")
                .and_then(Value::as_u64)
        })
}

fn status_values_match(spec: Option<&Value>, value: &Value) -> bool {
    match spec {
        Some(Value::Array(items)) => items.iter().any(|item| item == value),
        Some(expected) => expected == value,
        None => false,
    }
}

fn default_poll_state(response: &Value) -> Option<PollState> {
    if response.get("done").and_then(Value::as_bool) == Some(true) {
        return Some(PollState::Succeeded);
    }
    let status = response
        .get("status")
        .or_else(|| response.get("state"))
        .and_then(Value::as_str)?
        .to_ascii_lowercase();
    match status.as_str() {
        "queued" | "pending" | "running" | "in_progress" | "processing" => Some(PollState::Pending),
        "succeeded" | "completed" | "complete" | "done" | "success" => Some(PollState::Succeeded),
        "failed" | "cancelled" | "canceled" | "error" => Some(PollState::Failed),
        _ => None,
    }
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

fn render_submit_response(request: &ExecutionRequest) -> Result<Value, TemplateError> {
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
        .get("client_async_submit_response_template")
        .ok_or_else(|| {
            TemplateError::Invalid("client_async_submit_response_template is required".to_string())
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
