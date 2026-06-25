use crate::ctx::ReqCtx;
use base64::Engine;
use bytes::Bytes;
use futures_util::stream;
use gw_billing::formulas::{calculate_freeze_amount, TaskBillingConfig};
use gw_core::entity::{AsyncTaskBindingRevision, AsyncTaskEndpointRegistry};
use gw_core::error::codes;
use gw_core::ids::{KeyId, VendorId};
use gw_store::EntityStore;
use gw_task::{
    CancelReason, ClientMode, TaskContract, TaskError, TaskId, TaskKind, TaskRecord, TaskStatus,
    UpstreamMode,
};
use gw_task_runtime::binding_selector::{select_binding, BindingSelection, SelectedTarget};
use gw_task_runtime::client_mode::select_client_mode;
use gw_task_runtime::template_executor::{
    ExecutionIdentity, ExecutionRequest, ExecutionResult, NotWiredTransport, TemplateExecutor,
    UpstreamTransport,
};
use pingora::http::ResponseHeader;
use pingora::Result;
use pingora_proxy::Session;
use reqwest::header::HeaderMap;
use serde_json::{json, Map};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskEndpointCandidate {
    pub method: String,
    pub path: String,
    pub task_kind: String,
    pub model_path: Option<String>,
    pub default_model: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRequestKey {
    pub global_model_id: String,
    pub task_kind: String,
    pub endpoint_method: Option<String>,
    pub endpoint_path: Option<String>,
}

impl TaskRequestKey {
    #[must_use]
    pub fn new(global_model_id: impl Into<String>, task_kind: impl Into<String>) -> Self {
        Self {
            global_model_id: global_model_id.into(),
            task_kind: task_kind.into(),
            endpoint_method: None,
            endpoint_path: None,
        }
    }

    #[must_use]
    pub fn cache_key(&self) -> String {
        format!("{}:{}", self.global_model_id, self.task_kind)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTaskRuntime {
    pub profile: serde_json::Value,
    pub binding: serde_json::Value,
    pub profile_revision_id: String,
    pub binding_revision_id: String,
    pub effective_template_hash: String,
    pub upstream_base_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedExecutionModes {
    pub client_mode: ClientMode,
    pub upstream_mode: UpstreamMode,
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

pub fn resolve_task_request_key(
    candidate: &TaskEndpointCandidate,
    body: &serde_json::Value,
) -> std::result::Result<TaskRequestKey, TaskHttpError> {
    let extracted_model = candidate
        .model_path
        .as_deref()
        .and_then(|model_path| gw_task::template::eval_json_path(body, model_path).ok())
        .and_then(|value| value.as_str().map(str::to_string))
        .filter(|value| !value.is_empty());
    let model = extracted_model
        .or_else(|| candidate.default_model.clone())
        .filter(|value| !value.is_empty())
        .ok_or_else(TaskHttpError::model_required)?;

    Ok(TaskRequestKey {
        global_model_id: model,
        task_kind: candidate.task_kind.clone(),
        endpoint_method: Some(candidate.method.clone()),
        endpoint_path: Some(candidate.path.clone()),
    })
}

#[must_use]
pub fn should_parse_openai_compatible_client_request(candidate: &TaskEndpointCandidate) -> bool {
    candidate.model_path.is_some() || candidate.default_model.is_some()
}

pub async fn parse_openai_compatible_client_request(
    candidate: &TaskEndpointCandidate,
    content_type: Option<&str>,
    body: &[u8],
) -> std::result::Result<serde_json::Value, TaskHttpError> {
    let content_type = content_type.or(candidate.content_type.as_deref());
    if content_type
        .unwrap_or_default()
        .to_ascii_lowercase()
        .starts_with("multipart/form-data")
    {
        return parse_openai_multipart_client_request(content_type.unwrap_or_default(), body).await;
    }

    serde_json::from_slice::<serde_json::Value>(body)
        .map_err(|err| TaskHttpError::template_invalid(format!("invalid task json payload: {err}")))
}

async fn parse_openai_multipart_client_request(
    content_type: &str,
    body: &[u8],
) -> std::result::Result<serde_json::Value, TaskHttpError> {
    let boundary = multer::parse_boundary(content_type).map_err(|err| {
        TaskHttpError::template_invalid(format!("invalid multipart content-type: {err}"))
    })?;
    let stream =
        stream::once(
            async move { Ok::<Bytes, std::convert::Infallible>(Bytes::copy_from_slice(body)) },
        );
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut root = Map::new();
    let mut fields = Map::new();
    let mut files = Map::new();

    while let Some(field) = multipart.next_field().await.map_err(|err| {
        TaskHttpError::template_invalid(format!("invalid multipart payload: {err}"))
    })? {
        let Some(name) = field.name().map(str::to_string) else {
            continue;
        };
        let file_name = field.file_name().map(str::to_string);
        let field_content_type = field.content_type().map(ToString::to_string);
        let bytes = field.bytes().await.map_err(|err| {
            TaskHttpError::template_invalid(format!("invalid multipart field {name}: {err}"))
        })?;
        if let Some(file_name) = file_name {
            let file = json!({
                "filename": file_name,
                "content_type": field_content_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                "size": bytes.len(),
                "bytes_b64": base64::engine::general_purpose::STANDARD.encode(bytes.as_ref()),
            });
            insert_repeated(&mut root, &name, file.clone());
            push_repeated(&mut files, &name, file);
        } else {
            let value = String::from_utf8(bytes.to_vec()).map_err(|err| {
                TaskHttpError::template_invalid(format!(
                    "multipart field {name} is not valid utf-8: {err}"
                ))
            })?;
            insert_repeated(&mut root, &name, serde_json::Value::String(value.clone()));
            insert_repeated(&mut fields, &name, serde_json::Value::String(value));
        }
    }

    root.insert(
        "_multipart".to_string(),
        json!({
            "fields": fields,
            "files": files,
        }),
    );
    Ok(serde_json::Value::Object(root))
}

fn insert_repeated(
    target: &mut Map<String, serde_json::Value>,
    name: &str,
    value: serde_json::Value,
) {
    match target.get_mut(name) {
        Some(existing @ serde_json::Value::Array(_)) => {
            existing.as_array_mut().expect("array checked").push(value);
        }
        Some(existing) => {
            let first = std::mem::take(existing);
            *existing = serde_json::Value::Array(vec![first, value]);
        }
        None => {
            target.insert(name.to_string(), value);
        }
    }
}

fn push_repeated(
    target: &mut Map<String, serde_json::Value>,
    name: &str,
    value: serde_json::Value,
) {
    match target.get_mut(name) {
        Some(serde_json::Value::Array(items)) => items.push(value),
        Some(existing) => {
            let first = std::mem::take(existing);
            *existing = serde_json::Value::Array(vec![first, value]);
        }
        None => {
            target.insert(name.to_string(), serde_json::Value::Array(vec![value]));
        }
    }
}

pub async fn resolve_task_runtime(
    store: &Arc<EntityStore>,
    key: &TaskRequestKey,
) -> std::result::Result<ResolvedTaskRuntime, TaskHttpError> {
    let Some(index) = store
        .async_task_binding_index_or_load(&key.cache_key())
        .await
    else {
        return Err(TaskHttpError::profile_not_configured(format!(
            "async task binding index not configured for {}",
            key.cache_key()
        )));
    };

    let mut revisions = Vec::new();
    for revision_id in &index.value.binding_revision_ids {
        let Some(revision) = store.async_task_binding_revision_or_load(revision_id).await else {
            continue;
        };
        revisions.push((*revision.value).clone());
    }
    let revisions = endpoint_matching_revisions(store, key, revisions).await?;

    let target = selected_target_from_index(&index.value, &revisions, key);
    let selection = select_binding(&index.value, &revisions, &target)
        .map_err(|err| TaskHttpError::profile_not_configured(format!("{}: {err}", err.code())))?;

    match selection {
        BindingSelection::Published(binding_revision) => {
            let upstream_base_url = resolve_upstream_base_url(store, &binding_revision).await;
            let Some(profile_revision) = store
                .async_task_profile_revision_or_load(&binding_revision.profile_revision_id)
                .await
            else {
                return Err(TaskHttpError::profile_not_configured(format!(
                    "async task profile revision {} not configured",
                    binding_revision.profile_revision_id
                )));
            };
            Ok(ResolvedTaskRuntime {
                profile: profile_revision.value.profile.clone(),
                binding: binding_revision_to_value(&binding_revision),
                profile_revision_id: binding_revision.profile_revision_id,
                binding_revision_id: binding_revision.id,
                effective_template_hash: binding_revision.effective_template_hash,
                upstream_base_url,
            })
        }
        BindingSelection::Builtin {
            profile,
            binding,
            effective_template_hash,
        } => Ok(ResolvedTaskRuntime {
            profile,
            binding,
            profile_revision_id: "builtin:model_id_only".to_string(),
            binding_revision_id: "builtin:model_id_only".to_string(),
            effective_template_hash,
            upstream_base_url: None,
        }),
    }
}

async fn resolve_upstream_base_url(
    store: &Arc<EntityStore>,
    binding_revision: &AsyncTaskBindingRevision,
) -> Option<String> {
    if let Some(base_url) = binding_revision
        .target_snapshot
        .get("base_url")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
    {
        return Some(base_url.to_string());
    }
    let provider_key_id = binding_revision
        .provider_key_id
        .is_empty()
        .then_some(None)
        .unwrap_or_else(|| Some(KeyId::from(binding_revision.provider_key_id.as_str())))?;
    let vendor_key = store.vendor_key_or_load(&provider_key_id).await?;
    let vendor = store.vendor_or_load(&vendor_key.value.vendor_id).await?;
    if let Some(url) = vendor.value.base_urls.first() {
        return Some(url.as_str().trim_end_matches('/').to_string());
    }
    if binding_revision.provider_id.is_empty() {
        return None;
    }
    let vendor = store
        .vendor_or_load(&VendorId::from(binding_revision.provider_id.as_str()))
        .await?;
    vendor
        .value
        .base_urls
        .first()
        .map(|url| url.as_str().trim_end_matches('/').to_string())
}

pub fn resolve_execution_modes(
    profile: &serde_json::Value,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    query: &str,
    body: Option<&serde_json::Value>,
) -> std::result::Result<ResolvedExecutionModes, TaskHttpError> {
    let client_mode = select_client_mode(profile, method, path, headers, query, body).map_err(
        |err| match err {
            gw_task::template::TemplateError::ClientModeUnsupported(message) => {
                TaskHttpError::client_mode_unsupported(message)
            }
            other => TaskHttpError::template_invalid(format!("{}: {other}", other.code())),
        },
    )?;
    let upstream_mode = match profile
        .get("upstream_mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("sync")
    {
        "sync" => UpstreamMode::Sync,
        "async_lro" => UpstreamMode::AsyncLro,
        other => {
            return Err(TaskHttpError::template_invalid(format!(
                "unsupported upstream mode {other}"
            )))
        }
    };
    Ok(ResolvedExecutionModes {
        client_mode,
        upstream_mode,
    })
}

#[allow(clippy::too_many_arguments)]
pub async fn handle_openai_compatible_submit(
    runtime: ResolvedTaskRuntime,
    modes: ResolvedExecutionModes,
    client_request: serde_json::Value,
    transport: Arc<dyn UpstreamTransport>,
    user_id: &str,
    api_key_id: Option<&str>,
    trace_id: &str,
    request_id: &str,
) -> std::result::Result<TaskHttpResponse, TaskHttpError> {
    let task_kind = task_kind_from_profile(&runtime.profile)?;
    let executor = TemplateExecutor::new(transport);
    let execution = ExecutionRequest {
        identity: ExecutionIdentity {
            user_id: user_id.to_string(),
            api_key_id: api_key_id.unwrap_or_default().to_string(),
            request_id: request_id.to_string(),
            trace_id: trace_id.to_string(),
        },
        task_kind,
        client_mode: modes.client_mode,
        upstream_mode: modes.upstream_mode,
        profile: runtime.profile,
        binding: runtime.binding,
        client_request,
        task: json!({
            "id": request_id,
            "status": "queued",
            "profile_revision_id": runtime.profile_revision_id,
            "binding_revision_id": runtime.binding_revision_id,
            "effective_template_hash": runtime.effective_template_hash,
        }),
    };
    match executor
        .execute_inline(execution)
        .await
        .map_err(|err| TaskHttpError::template_invalid(format!("{}: {err}", err.code())))?
    {
        ExecutionResult::Completed { response, .. } => Ok(TaskHttpResponse {
            status: 200,
            content_type: "application/json",
            headers: BTreeMap::new(),
            body: response,
        }),
        ExecutionResult::Submitted { response, .. } => Ok(TaskHttpResponse {
            status: 202,
            content_type: "application/json",
            headers: BTreeMap::new(),
            body: response,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn submit_openai_compatible_task<C: TaskContract + Sync>(
    contract: &C,
    runtime: ResolvedTaskRuntime,
    modes: ResolvedExecutionModes,
    client_request: serde_json::Value,
    user_id: &str,
    api_key_id: Option<&str>,
    trace_id: &str,
    request_id: &str,
    billing_config: Option<TaskBillingConfig>,
    available_balance_micro: Option<i64>,
) -> std::result::Result<TaskHttpResponse, TaskHttpError> {
    let task_kind = task_kind_from_profile(&runtime.profile)?;
    let now_ms = now_ms();
    let task_id = TaskId::new_v4();
    let frozen_micro = billing_config
        .as_ref()
        .map(|config| calculate_freeze_amount(task_kind, config))
        .unwrap_or(0);
    if frozen_micro > 0 {
        contract
            .freeze_task(user_id, &task_id, frozen_micro, available_balance_micro)
            .await
            .map_err(TaskHttpError::from)?;
    }

    let input_payload = json!({
        "client_request": client_request,
        "profile": runtime.profile,
        "binding": runtime.binding,
        "profile_revision_id": runtime.profile_revision_id,
        "binding_revision_id": runtime.binding_revision_id,
        "effective_template_hash": runtime.effective_template_hash,
        "upstream_base_url": runtime.upstream_base_url,
    });
    let model = input_payload["client_request"]
        .get("model")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let profile_revision_id = input_payload["profile_revision_id"]
        .as_str()
        .map(str::to_string);
    let binding_revision_id = input_payload["binding_revision_id"]
        .as_str()
        .map(str::to_string);
    let effective_template_hash = input_payload["effective_template_hash"]
        .as_str()
        .map(str::to_string);
    let binding = &input_payload["binding"];
    let record = TaskRecord {
        id: task_id,
        schema_version: 2,
        kind: task_kind,
        status: TaskStatus::Queued,
        user_id: user_id.to_string(),
        child_id: None,
        api_key_id: api_key_id.unwrap_or_default().to_string(),
        vendor_id: binding
            .get("provider_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        model: model.clone(),
        client_mode: modes.client_mode,
        upstream_mode: modes.upstream_mode,
        global_model_id: binding
            .get("global_model_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or(model.clone()),
        provider_model_id: binding
            .get("provider_model_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        endpoint_id: binding
            .get("endpoint_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        provider_key_id: binding
            .get("provider_key_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        profile_id: input_payload["profile"]
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        profile_revision_id,
        profile_version: binding
            .get("profile_version")
            .and_then(serde_json::Value::as_u64),
        binding_id: binding
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        binding_revision_id,
        binding_version: binding.get("version").and_then(serde_json::Value::as_u64),
        effective_template_hash,
        poll_context: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
        submitted_ts_ms: now_ms,
        expires_at_ms: now_ms + 86_400_000,
        claimed_ts_ms: None,
        ended_ts_ms: None,
        lease_until_ms: None,
        worker_id: None,
        trace_id: Some(trace_id.to_string()),
        request_id: Some(request_id.to_string()),
        provider_task_id: None,
        provider_first_response_ms: None,
        input_payload: input_payload.clone(),
        output_payload: None,
        error_message: None,
        frozen_micro,
        actual_micro: None,
        duration_seconds: None,
        billing_record_id: None,
    };
    contract.submit(record).await.map_err(TaskHttpError::from)?;

    let executor = TemplateExecutor::new(Arc::new(NotWiredTransport));
    let execution = ExecutionRequest {
        identity: ExecutionIdentity {
            user_id: user_id.to_string(),
            api_key_id: api_key_id.unwrap_or_default().to_string(),
            request_id: request_id.to_string(),
            trace_id: trace_id.to_string(),
        },
        task_kind,
        client_mode: ClientMode::Async,
        upstream_mode: modes.upstream_mode,
        profile: input_payload["profile"].clone(),
        binding: input_payload["binding"].clone(),
        client_request: input_payload["client_request"].clone(),
        task: json!({"id": task_id.to_string(), "status": "queued"}),
    };
    match executor
        .execute_inline(execution)
        .await
        .map_err(|err| TaskHttpError::template_invalid(format!("{}: {err}", err.code())))?
    {
        ExecutionResult::Submitted { response, .. } => Ok(TaskHttpResponse {
            status: 202,
            content_type: "application/json",
            headers: BTreeMap::new(),
            body: response,
        }),
        ExecutionResult::Completed { response, .. } => Ok(TaskHttpResponse {
            status: 200,
            content_type: "application/json",
            headers: BTreeMap::new(),
            body: response,
        }),
    }
}

fn task_kind_from_profile(
    profile: &serde_json::Value,
) -> std::result::Result<TaskKind, TaskHttpError> {
    match profile
        .get("task_kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("generic")
    {
        "image" => Ok(TaskKind::Image),
        "video" => Ok(TaskKind::Video),
        "audio" => Ok(TaskKind::Audio),
        "batch" => Ok(TaskKind::Batch),
        "lro_responses" => Ok(TaskKind::LroResponses),
        "embedding" => Ok(TaskKind::Embedding),
        "generic" => Ok(TaskKind::Generic),
        other => Err(TaskHttpError::template_invalid(format!(
            "unsupported task kind {other}"
        ))),
    }
}

fn selected_target_from_index(
    index: &gw_core::entity::AsyncTaskBindingIndex,
    revisions: &[AsyncTaskBindingRevision],
    key: &TaskRequestKey,
) -> SelectedTarget {
    let best = revisions.iter().max_by(|left, right| {
        left.priority
            .cmp(&right.priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    SelectedTarget {
        task_kind: index.task_kind.clone(),
        global_model_id: index.global_model_id.clone(),
        provider_model_id: best
            .map(|revision| revision.provider_model_id.clone())
            .unwrap_or_else(|| key.global_model_id.clone()),
        endpoint_id: best
            .map(|revision| revision.endpoint_id.clone())
            .unwrap_or_default(),
        api_format: best
            .map(|revision| revision.api_format.clone())
            .unwrap_or_else(|| "openai".to_string()),
        provider_key_id: best.and_then(|revision| {
            (!revision.provider_key_id.is_empty()).then(|| revision.provider_key_id.clone())
        }),
        openai_compatible: true,
    }
}

async fn endpoint_matching_revisions(
    store: &Arc<EntityStore>,
    key: &TaskRequestKey,
    revisions: Vec<AsyncTaskBindingRevision>,
) -> std::result::Result<Vec<AsyncTaskBindingRevision>, TaskHttpError> {
    let (Some(method), Some(path)) = (key.endpoint_method.as_deref(), key.endpoint_path.as_deref())
    else {
        return Ok(revisions);
    };

    let mut matched = Vec::new();
    for revision in &revisions {
        let Some(profile_revision) = store
            .async_task_profile_revision_or_load(&revision.profile_revision_id)
            .await
        else {
            continue;
        };
        if profile_matches_endpoint(&profile_revision.value.profile, method, path) {
            matched.push(revision.clone());
        }
    }

    if matched.is_empty() {
        return Err(TaskHttpError::profile_not_configured(format!(
            "async task profile is not configured for {} {} and {}",
            method,
            path,
            key.cache_key()
        )));
    }
    Ok(matched)
}

fn profile_matches_endpoint(profile: &serde_json::Value, method: &str, path: &str) -> bool {
    let endpoint = profile
        .get("client_request_template")
        .and_then(|value| value.get("endpoint"));
    let Some(endpoint) = endpoint else {
        return false;
    };
    let endpoint_method = endpoint
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let endpoint_path = endpoint
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    endpoint_method == method && endpoint_path == path
}

fn binding_revision_to_value(revision: &AsyncTaskBindingRevision) -> serde_json::Value {
    let provider_model_name = revision
        .provider_model_name
        .is_empty()
        .then(|| {
            revision
                .target_snapshot
                .get("provider_model_name")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(&revision.provider_model_id)
                .to_string()
        })
        .unwrap_or_else(|| revision.provider_model_name.clone());
    json!({
        "id": revision.binding_id,
        "revision_id": revision.id,
        "version": revision.version,
        "profile_id": revision.profile_id,
        "profile_revision_id": revision.profile_revision_id,
        "profile_version": revision.profile_version,
        "task_kind": revision.task_kind,
        "global_model_id": revision.global_model_id,
        "provider_id": revision.provider_id,
        "provider_model_id": revision.provider_model_id,
        "provider_model_name": provider_model_name,
        "endpoint_id": revision.endpoint_id,
        "api_format": revision.api_format,
        "provider_key_id": revision.provider_key_id,
        "priority": revision.priority,
        "target_snapshot": revision.target_snapshot,
        "overrides_snapshot": revision.overrides_snapshot,
        "effective_template_hash": revision.effective_template_hash,
    })
}

pub fn detect_openai_compatible_task_endpoint(
    task_enabled: bool,
    method: &str,
    path: &str,
    content_type: &str,
) -> Option<TaskEndpointCandidate> {
    if !task_enabled {
        return None;
    }

    let (task_kind, model_path) = match (method, path) {
        ("POST", "/v1/images/generations")
        | ("POST", "/v1/images/edits")
        | ("POST", "/v1/images/variations") => ("image", Some("$.model")),
        ("POST", "/v1/embeddings") => ("embedding", Some("$.model")),
        ("POST", "/v1/videos")
        | ("POST", "/v1/videos/edits")
        | ("POST", "/v1/videos/extensions")
        | ("POST", "/v1/videos/characters") => ("video", Some("$.model")),
        ("POST", path) if openai_video_remix_path(path) => ("video", Some("$.model")),
        ("GET", path) if openai_video_resource_path(path) => ("video", None),
        _ => return None,
    };

    Some(TaskEndpointCandidate {
        method: method.to_string(),
        path: path.to_string(),
        task_kind: task_kind.to_string(),
        model_path: model_path.map(str::to_string),
        default_model: None,
        content_type: if content_type.is_empty() {
            None
        } else {
            Some(content_type.to_string())
        },
    })
}

fn openai_video_resource_path(path: &str) -> bool {
    let segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    matches!(
        segments.as_slice(),
        ["v1", "videos", video_id] if !video_id.is_empty()
    ) || matches!(
        segments.as_slice(),
        ["v1", "videos", video_id, "content"] if !video_id.is_empty()
    )
}

fn openai_video_remix_path(path: &str) -> bool {
    let segments: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    matches!(
        segments.as_slice(),
        ["v1", "videos", video_id, "remix"] if !video_id.is_empty()
    )
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

pub fn published_task_endpoint_candidate(
    registry: &AsyncTaskEndpointRegistry,
    method: &str,
    path: &str,
    content_type: &str,
) -> Option<TaskEndpointCandidate> {
    registry.routes.iter().find_map(|route| {
        let route_method = route.get("method").and_then(serde_json::Value::as_str)?;
        let route_path = route.get("path").and_then(serde_json::Value::as_str)?;
        if route_method != method || route_path != path {
            return None;
        }
        if route_method == "GET" && !openai_video_resource_path(path) {
            return None;
        }
        let task_kind = route.get("task_kind").and_then(serde_json::Value::as_str)?;
        Some(TaskEndpointCandidate {
            method: method.to_string(),
            path: path.to_string(),
            task_kind: task_kind.to_string(),
            model_path: route
                .get("model_path")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            default_model: route
                .get("default_model")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            content_type: if content_type.is_empty() {
                route
                    .get("content_type")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            } else {
                Some(content_type.to_string())
            },
        })
    })
}

pub fn mark_task_context(
    ctx: &mut ReqCtx,
    task_enabled: bool,
    method: &str,
    path: &str,
    content_type: Option<&str>,
) -> bool {
    if let Some(action) = detect_task_shape(task_enabled, method, path, None) {
        ctx.is_task = true;
        ctx.task_action = Some(action);
        true
    } else if let Some(candidate) = detect_openai_compatible_task_endpoint(
        task_enabled,
        method,
        path,
        content_type.unwrap_or_default(),
    ) {
        ctx.is_task = true;
        ctx.task_endpoint_candidate = Some(candidate);
        true
    } else {
        false
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskHttpResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: BTreeMap<&'static str, String>,
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

    fn model_required() -> Self {
        Self {
            status: 400,
            code: codes::GW_TASK_MODEL_REQUIRED,
            message: Cow::Borrowed("model is required for task endpoint"),
        }
    }

    fn profile_not_configured(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: 404,
            code: codes::GW_TASK_PROFILE_NOT_CONFIGURED,
            message: message.into(),
        }
    }

    fn client_mode_unsupported(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: 400,
            code: codes::GW_TASK_CLIENT_MODE_UNSUPPORTED,
            message: message.into(),
        }
    }

    fn template_invalid(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: 400,
            code: codes::GW_TASK_TEMPLATE_INVALID,
            message: message.into(),
        }
    }

    fn artifact_not_found(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            status: 404,
            code: codes::GW_TASK_ARTIFACT_NOT_FOUND,
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
        headers: BTreeMap::new(),
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
        headers: BTreeMap::new(),
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
        headers: BTreeMap::new(),
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
        headers: BTreeMap::new(),
        body: serde_json::to_value(record)
            .map_err(|err| TaskHttpError::submit_failed(format!("serialize task: {err}")))?,
    })
}

pub async fn load_openai_video_endpoint<C: TaskContract + Sync>(
    contract: &C,
    video_id: &str,
    user_id: &str,
) -> std::result::Result<TaskHttpResponse, TaskHttpError> {
    let id = parse_task_id(video_id)?;
    let record = contract.load(&id).await.map_err(TaskHttpError::from)?;
    let record = record.ok_or_else(TaskHttpError::not_found)?;
    ensure_task_owner(&record, user_id)?;
    Ok(TaskHttpResponse {
        status: 200,
        content_type: "application/json",
        headers: BTreeMap::new(),
        body: openai_video_object(&record),
    })
}

pub async fn load_openai_video_content_endpoint<C: TaskContract + Sync>(
    contract: &C,
    video_id: &str,
    user_id: &str,
) -> std::result::Result<TaskHttpResponse, TaskHttpError> {
    let id = parse_task_id(video_id)?;
    let record = contract.load(&id).await.map_err(TaskHttpError::from)?;
    let record = record.ok_or_else(TaskHttpError::not_found)?;
    ensure_task_owner(&record, user_id)?;
    if !record.status.is_terminal() {
        return Err(TaskHttpError::artifact_not_found(
            "video content is not available before the task reaches a terminal state",
        ));
    }
    let Some(url) = openai_video_content_url(&record) else {
        return Err(TaskHttpError::artifact_not_found(
            "video content artifact not found",
        ));
    };
    Ok(TaskHttpResponse {
        status: 302,
        content_type: "application/octet-stream",
        headers: BTreeMap::from([("Location", url)]),
        body: serde_json::Value::Null,
    })
}

fn ensure_task_owner(record: &TaskRecord, user_id: &str) -> std::result::Result<(), TaskHttpError> {
    if record.user_id == user_id {
        Ok(())
    } else {
        Err(TaskHttpError::not_found())
    }
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
    for (name, value) in response.headers {
        header.insert_header(name, value)?;
    }
    session
        .write_response_header(Box::new(header), false)
        .await?;
    session.write_response_body(Some(body), true).await?;
    Ok(())
}

fn parse_task_id(task_id: &str) -> std::result::Result<TaskId, TaskHttpError> {
    TaskId::from_str(task_id).map_err(|_| TaskHttpError::not_found())
}

fn openai_video_object(record: &TaskRecord) -> serde_json::Value {
    let client_request = record.input_payload.get("client_request");
    let prompt = client_request
        .and_then(|value| value.get("prompt"))
        .or_else(|| record.input_payload.get("prompt"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let mut body = json!({
        "id": record.id.to_string(),
        "object": "video",
        "created_at": record.created_at_ms / 1000,
        "status": openai_video_status(record.status),
        "model": record.model.clone().or_else(|| record.global_model_id.clone()),
        "progress": openai_video_progress(record.status),
        "prompt": prompt,
    });
    if let Some(error) = record.error_message.as_deref() {
        body["error"] = json!({"message": error});
    }
    if let Some(url) = openai_video_content_url(record) {
        body["content_url"] = serde_json::Value::String(url);
    }
    body
}

fn openai_video_status(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending | TaskStatus::Queued => "queued",
        TaskStatus::Running | TaskStatus::Cancelling => "in_progress",
        TaskStatus::Succeeded => "completed",
        TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::TimedOut => "failed",
    }
}

fn openai_video_progress(status: TaskStatus) -> u64 {
    match status {
        TaskStatus::Succeeded => 100,
        TaskStatus::Pending | TaskStatus::Queued => 0,
        TaskStatus::Running | TaskStatus::Cancelling => 50,
        TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::TimedOut => 0,
    }
}

fn openai_video_content_url(record: &TaskRecord) -> Option<String> {
    let output = record.output_payload.as_ref()?;
    find_first_string_path(
        output,
        &[
            &["client_final_response", "data", "0", "url"],
            &["client_final_response", "url"],
            &["client_final_response", "content_url"],
            &["provider_final_response", "url"],
            &["result", "url"],
            &["url"],
            &["content_url"],
        ],
    )
}

fn find_first_string_path(value: &serde_json::Value, paths: &[&[&str]]) -> Option<String> {
    paths.iter().find_map(|path| {
        let mut current = value;
        for segment in *path {
            current = if let Ok(index) = segment.parse::<usize>() {
                current.as_array()?.get(index)?
            } else {
                current.get(*segment)?
            };
        }
        current.as_str().map(str::to_string)
    })
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
            "embedding" => TaskKind::Embedding,
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
