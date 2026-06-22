use std::collections::BTreeMap;

use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub type Context = Map<String, Value>;

pub const CODE_TEMPLATE_INVALID: &str = "GW_TASK_TEMPLATE_INVALID";
pub const CODE_TEMPLATE_RENDER_FAILED: &str = "GW_TASK_TEMPLATE_RENDER_FAILED";
pub const CODE_CLIENT_MODE_UNSUPPORTED: &str = "GW_TASK_CLIENT_MODE_UNSUPPORTED";
pub const CODE_PROFILE_NOT_CONFIGURED: &str = "GW_TASK_PROFILE_NOT_CONFIGURED";
pub const CODE_BINDING_CONFLICT: &str = "GW_TASK_BINDING_CONFLICT";

pub const BUILTIN_MODEL_ID_ONLY: &str = include_str!("templates/builtin_model_id_only.json");

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TemplateError {
    #[error("async task template invalid: {0}")]
    Invalid(String),
    #[error("async task template render failed: {0}")]
    RenderFailed(String),
    #[error("async task client mode unsupported: {0}")]
    ClientModeUnsupported(String),
    #[error("async task profile not configured: {0}")]
    ProfileNotConfigured(String),
    #[error("async task binding conflict: {0}")]
    BindingConflict(String),
}

impl TemplateError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => CODE_TEMPLATE_INVALID,
            Self::RenderFailed(_) => CODE_TEMPLATE_RENDER_FAILED,
            Self::ClientModeUnsupported(_) => CODE_CLIENT_MODE_UNSUPPORTED,
            Self::ProfileNotConfigured(_) => CODE_PROFILE_NOT_CONFIGURED,
            Self::BindingConflict(_) => CODE_BINDING_CONFLICT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderLimits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_map_items: usize,
}

impl Default for RenderLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 20 << 20,
            max_output_bytes: 20 << 20,
            max_nodes: 50_000,
            max_depth: 128,
            max_map_items: 1_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderedRequest {
    pub method: String,
    pub path: String,
    pub headers: Map<String, Value>,
    pub body: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RenderFixtureResult {
    pub effective_template_hash: String,
    pub normalized_client: Value,
    pub rendered_upstream_request: RenderedRequest,
    pub internal_result: Value,
    pub client_sync_response: Value,
}

#[derive(Clone, Debug)]
struct Budget {
    limits: RenderLimits,
    nodes: usize,
}

pub fn render(ctx: &Context, tmpl: &Value, limits: RenderLimits) -> Result<Value, TemplateError> {
    let input_bytes = serde_json::to_vec(tmpl)
        .map_err(|err| TemplateError::RenderFailed(format!("encode template: {err}")))?
        .len();
    if input_bytes > limits.max_input_bytes {
        return Err(TemplateError::RenderFailed(format!(
            "template input exceeds {} bytes",
            limits.max_input_bytes
        )));
    }

    let mut budget = Budget { limits, nodes: 0 };
    render_inner(ctx, tmpl, &mut budget, 0)
}

pub fn render_fixture(fixture: &Value) -> Result<RenderFixtureResult, TemplateError> {
    let profile = object_at(fixture, "profile")?;
    let binding = object_at(fixture, "binding")?;
    let client_request = object_at(fixture, "client_request")?;

    let upstream_request_template =
        object_at(&Value::Object(profile.clone()), "upstream_request_template")?;
    validate_headers(
        upstream_request_template
            .get("headers")
            .and_then(Value::as_object),
    )?;

    let client_request_template = profile
        .get("client_request_template")
        .and_then(Value::as_object)
        .ok_or_else(|| TemplateError::Invalid("client_request_template is required".to_owned()))?;
    validate_required(client_request_template.get("required"), &client_request)?;
    let normalized_client = normalize_client(client_request_template, &client_request)?;

    let mut ctx = Context::new();
    ctx.insert("binding".to_owned(), Value::Object(binding.clone()));
    ctx.insert(
        "client".to_owned(),
        Value::Object(normalized_client.clone()),
    );
    ctx.insert(
        "task".to_owned(),
        fixture
            .get("task")
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new())),
    );

    let method = upstream_request_template
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let path = upstream_request_template
        .get("path")
        .map(|value| render(&ctx, value, RenderLimits::default()))
        .transpose()?
        .map(|value| value_as_string(&value))
        .unwrap_or_default();
    let headers = upstream_request_template
        .get("headers")
        .map(|value| render(&ctx, value, RenderLimits::default()))
        .transpose()?
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let body = upstream_request_template
        .get("body")
        .map(|value| render(&ctx, value, RenderLimits::default()))
        .transpose()?
        .unwrap_or(Value::Null);

    let submit_response = fixture
        .get("mock_upstream_submit_response")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let poll_response = fixture
        .get("mock_upstream_poll_response")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let upstream_mode = profile
        .get("upstream_mode")
        .and_then(Value::as_str)
        .unwrap_or("sync");
    let upstream_response = if upstream_mode == "async_lro" && non_empty_object(&poll_response) {
        poll_response
    } else {
        submit_response
    };

    let result = render_result(
        profile
            .get("upstream_response_template")
            .and_then(|value| value.get("result")),
        &upstream_response,
    )?;
    ctx.insert("result".to_owned(), result.clone());
    let client_sync_response = profile
        .get("client_sync_response_template")
        .map(|value| render(&ctx, value, RenderLimits::default()))
        .transpose()?
        .unwrap_or(Value::Null);

    Ok(RenderFixtureResult {
        effective_template_hash: hash_effective_template(&serde_json::json!({
            "profile": profile,
            "binding": binding,
        })),
        normalized_client: Value::Object(normalized_client),
        rendered_upstream_request: RenderedRequest {
            method,
            path,
            headers,
            body,
        },
        internal_result: result,
        client_sync_response,
    })
}

pub fn hash_effective_template(value: &Value) -> String {
    let canonical = canonical_json(value);
    let sum = Sha256::digest(canonical.as_bytes());
    format!("sha256:{}", hex::encode(sum))
}

pub fn eval_json_path(root: &Value, path: &str) -> Result<Value, TemplateError> {
    if path == "$" {
        return Ok(root.clone());
    }
    let suffix = path
        .strip_prefix("$.")
        .ok_or_else(|| TemplateError::RenderFailed(format!("unsupported jsonpath {path:?}")))?;
    eval_path(root, suffix)
}

pub fn eval_context_path(ctx: &Context, path: &str) -> Result<Value, TemplateError> {
    eval_path(&Value::Object(ctx.clone()), path)
}

fn render_inner(
    ctx: &Context,
    tmpl: &Value,
    budget: &mut Budget,
    depth: usize,
) -> Result<Value, TemplateError> {
    if depth > budget.limits.max_depth {
        return Err(TemplateError::RenderFailed(format!(
            "template depth exceeds {}",
            budget.limits.max_depth
        )));
    }
    budget.nodes += 1;
    if budget.nodes > budget.limits.max_nodes {
        return Err(TemplateError::RenderFailed(format!(
            "template nodes exceed {}",
            budget.limits.max_nodes
        )));
    }

    let rendered = match tmpl {
        Value::Null | Value::Bool(_) | Value::Number(_) => tmpl.clone(),
        Value::String(text) => render_string(ctx, text)?,
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(render_inner(ctx, item, budget, depth + 1)?);
            }
            Value::Array(out)
        }
        Value::Object(obj) => {
            if let Some(expr) = obj.get("$expr").and_then(Value::as_str) {
                let got = match eval_context_path(ctx, expr) {
                    Ok(value) => value,
                    Err(err) => {
                        if let Some(default) = obj.get("default") {
                            default.clone()
                        } else {
                            return Err(err);
                        }
                    }
                };
                cast_value(
                    got,
                    obj.get("as").and_then(Value::as_str).unwrap_or_default(),
                )?
            } else if let Some(path) = obj.get("$path").and_then(Value::as_str) {
                eval_json_path(&Value::Object(ctx.clone()), path)?
            } else if let Some(map_spec) = obj.get("$map").and_then(Value::as_object) {
                render_map(ctx, map_spec, budget, depth + 1)?
            } else {
                let mut out = Map::with_capacity(obj.len());
                for (key, item) in obj {
                    out.insert(key.clone(), render_inner(ctx, item, budget, depth + 1)?);
                }
                Value::Object(out)
            }
        }
    };

    ensure_output_size(&rendered, budget.limits.max_output_bytes)?;
    Ok(rendered)
}

fn render_map(
    ctx: &Context,
    spec: &Map<String, Value>,
    budget: &mut Budget,
    depth: usize,
) -> Result<Value, TemplateError> {
    let source_tmpl = spec
        .get("source")
        .ok_or_else(|| TemplateError::RenderFailed("$map source is required".to_owned()))?;
    let source = render_inner(ctx, source_tmpl, budget, depth + 1)?;
    let items = source
        .as_array()
        .ok_or_else(|| TemplateError::RenderFailed("$map source must be array".to_owned()))?;
    if items.len() > budget.limits.max_map_items {
        return Err(TemplateError::RenderFailed(format!(
            "$map source length {} exceeds {}",
            items.len(),
            budget.limits.max_map_items
        )));
    }
    let alias = spec
        .get("as")
        .and_then(Value::as_str)
        .ok_or_else(|| TemplateError::RenderFailed("$map as is required".to_owned()))?;
    let template = spec.get("template").unwrap_or(&Value::Null);

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let mut child = ctx.clone();
        child.insert(alias.to_owned(), item.clone());
        out.push(render_inner(&child, template, budget, depth + 1)?);
    }
    Ok(Value::Array(out))
}

fn render_string(ctx: &Context, text: &str) -> Result<Value, TemplateError> {
    let matches = interpolation_matches(text);
    if matches.is_empty() {
        return Ok(Value::String(text.to_owned()));
    }
    if matches.len() == 1 && matches[0].0 == 0 && matches[0].1 == text.len() {
        return eval_context_path(ctx, matches[0].2.trim());
    }

    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end, expr) in matches {
        out.push_str(&text[last..start]);
        if let Ok(value) = eval_context_path(ctx, expr.trim()) {
            out.push_str(&value_as_string(&value));
        }
        last = end;
    }
    out.push_str(&text[last..]);
    Ok(Value::String(out))
}

fn interpolation_matches(text: &str) -> Vec<(usize, usize, &str)> {
    let mut out = Vec::new();
    let mut offset = 0;
    while let Some(start_rel) = text[offset..].find("{{") {
        let start = offset + start_rel;
        let expr_start = start + 2;
        let Some(end_rel) = text[expr_start..].find("}}") else {
            break;
        };
        let expr_end = expr_start + end_rel;
        let end = expr_end + 2;
        out.push((start, end, &text[expr_start..expr_end]));
        offset = end;
    }
    out
}

fn cast_value(value: Value, as_type: &str) -> Result<Value, TemplateError> {
    match as_type {
        "" | "json" => Ok(value),
        "string" => Ok(Value::String(value_as_string(&value))),
        "number" => match value {
            Value::Number(_) => Ok(value),
            Value::String(text) => {
                let parsed = text.parse::<f64>().map_err(|err| {
                    TemplateError::RenderFailed(format!("cannot cast string to number: {err}"))
                })?;
                Number::from_f64(parsed)
                    .map(Value::Number)
                    .ok_or_else(|| TemplateError::RenderFailed("invalid number".to_owned()))
            }
            other => Err(TemplateError::RenderFailed(format!(
                "cannot cast {} to number",
                type_name(&other)
            ))),
        },
        "bool" => match value {
            Value::Bool(_) => Ok(value),
            other => Err(TemplateError::RenderFailed(format!(
                "cannot cast {} to bool",
                type_name(&other)
            ))),
        },
        other => Err(TemplateError::RenderFailed(format!(
            "unsupported cast {other:?}"
        ))),
    }
}

fn normalize_client(
    spec: &Map<String, Value>,
    req: &Map<String, Value>,
) -> Result<Map<String, Value>, TemplateError> {
    let mut out = Map::new();
    let Some(normalize) = spec.get("normalize").and_then(Value::as_object) else {
        return Ok(out);
    };
    let req_value = Value::Object(req.clone());
    for (target, source) in normalize {
        let value = normalize_value(source, &req_value)?;
        set_dotted(
            &mut out,
            target.strip_prefix("client.").unwrap_or(target),
            value,
        );
    }
    Ok(out)
}

fn normalize_value(source: &Value, req: &Value) -> Result<Value, TemplateError> {
    if let Some(path) = source.as_str() {
        return eval_json_path(req, path);
    }
    if let Some(obj) = source.as_object() {
        let path = obj.get("value").and_then(Value::as_str).ok_or_else(|| {
            TemplateError::RenderFailed("normalize value path is required".to_owned())
        })?;
        return match eval_json_path(req, path) {
            Ok(value) => Ok(value),
            Err(err) => obj.get("default").cloned().ok_or(err),
        };
    }
    Ok(source.clone())
}

fn render_result(spec: Option<&Value>, upstream: &Value) -> Result<Value, TemplateError> {
    let mut out = Map::new();
    let Some(spec) = spec.and_then(Value::as_object) else {
        return Ok(Value::Object(out));
    };

    for (target, source) in spec {
        let Some(path) = source.as_str() else {
            continue;
        };
        let value = eval_json_path(upstream, path)?;
        set_dotted(&mut out, target, value);
    }
    Ok(Value::Object(out))
}

fn validate_required(
    required: Option<&Value>,
    req: &Map<String, Value>,
) -> Result<(), TemplateError> {
    let Some(required) = required.and_then(Value::as_array) else {
        return Ok(());
    };
    for field in required {
        let Some(field) = field.as_str() else {
            continue;
        };
        if !req.contains_key(field) {
            return Err(TemplateError::RenderFailed(format!("{field} is required")));
        }
    }
    Ok(())
}

fn validate_headers(headers: Option<&Map<String, Value>>) -> Result<(), TemplateError> {
    let Some(headers) = headers else {
        return Ok(());
    };
    for key in headers.keys() {
        let lower = key.to_ascii_lowercase();
        let blocked = matches!(
            lower.as_str(),
            "authorization"
                | "proxy-authorization"
                | "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
        ) || lower.starts_with("x-egress-");
        if blocked {
            return Err(TemplateError::Invalid(format!(
                "header {key} is managed by the gateway"
            )));
        }
    }
    Ok(())
}

fn eval_path(root: &Value, path: &str) -> Result<Value, TemplateError> {
    let parts = split_path(path);
    eval_path_parts(root, &parts)
}

fn eval_path_parts(root: &Value, parts: &[String]) -> Result<Value, TemplateError> {
    let mut cur = root.clone();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let (field, indexes) = parse_indexes(part);
        if !field.is_empty() {
            let obj = cur
                .as_object()
                .ok_or_else(|| TemplateError::RenderFailed(format!("{field} is not an object")))?;
            cur = obj
                .get(&field)
                .cloned()
                .ok_or_else(|| TemplateError::RenderFailed(format!("{field} is required")))?;
        }
        for (index_pos, index) in indexes.iter().enumerate() {
            let arr = cur
                .as_array()
                .ok_or_else(|| TemplateError::RenderFailed(format!("{part} is not an array")))?;
            if index == "*" {
                let mut out = Vec::with_capacity(arr.len());
                let remaining_indexes = &indexes[index_pos + 1..];
                let remaining_parts = &parts[i + 1..];
                for item in arr {
                    let mut next = item.clone();
                    for next_index in remaining_indexes {
                        let next_arr = next.as_array().ok_or_else(|| {
                            TemplateError::RenderFailed(format!("{part} is not an array"))
                        })?;
                        let n = parse_index(next_index, next_arr.len())?;
                        next = next_arr[n].clone();
                    }
                    if !remaining_parts.is_empty() {
                        next = eval_path_parts(&next, remaining_parts)?;
                    }
                    out.push(next);
                }
                return Ok(Value::Array(out));
            }
            let n = parse_index(index, arr.len())?;
            cur = arr[n].clone();
        }
    }
    Ok(cur)
}

fn split_path(path: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for ch in path.chars() {
        match ch {
            '.' if depth == 0 => {
                parts.push(std::mem::take(&mut current));
            }
            '[' => {
                depth += 1;
                current.push(ch);
            }
            ']' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            _ => current.push(ch),
        }
    }
    parts.push(current);
    parts
}

fn parse_indexes(part: &str) -> (String, Vec<String>) {
    let Some((field, rest)) = part.split_once('[') else {
        return (part.to_owned(), Vec::new());
    };
    let mut indexes = Vec::new();
    let mut remaining = rest;
    loop {
        let Some((idx, tail)) = remaining.split_once(']') else {
            break;
        };
        indexes.push(idx.to_owned());
        let Some(next) = tail.strip_prefix('[') else {
            break;
        };
        remaining = next;
    }
    (field.to_owned(), indexes)
}

fn parse_index(index: &str, len: usize) -> Result<usize, TemplateError> {
    let n = index
        .parse::<usize>()
        .map_err(|_| TemplateError::RenderFailed(format!("array index {index} out of range")))?;
    if n >= len {
        return Err(TemplateError::RenderFailed(format!(
            "array index {index} out of range"
        )));
    }
    Ok(n)
}

fn set_dotted(out: &mut Map<String, Value>, path: &str, value: Value) {
    let mut parts = path.split('.').peekable();
    let mut cur = out;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            cur.insert(part.to_owned(), value);
            return;
        }
        let entry = cur
            .entry(part.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        cur = entry.as_object_mut().expect("object inserted");
    }
}

fn object_at(value: &Value, key: &str) -> Result<Map<String, Value>, TemplateError> {
    value
        .get(key)
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| TemplateError::Invalid(format!("{key} is required")))
}

fn ensure_output_size(value: &Value, limit: usize) -> Result<(), TemplateError> {
    let size = serde_json::to_vec(value)
        .map_err(|err| TemplateError::RenderFailed(format!("encode rendered value: {err}")))?
        .len();
    if size > limit {
        return Err(TemplateError::RenderFailed(format!(
            "rendered output exceeds {limit} bytes"
        )));
    }
    Ok(())
}

fn non_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|obj| !obj.is_empty())
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn value_as_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => v.clone(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => serde_json::to_string(v).expect("string serializes"),
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        }
        Value::Object(obj) => {
            let sorted = obj
                .iter()
                .map(|(key, value)| (key, value))
                .collect::<BTreeMap<_, _>>();
            let inner = sorted
                .into_iter()
                .map(|(key, value)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("key serializes"),
                        canonical_json(value)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{inner}}}")
        }
    }
}
