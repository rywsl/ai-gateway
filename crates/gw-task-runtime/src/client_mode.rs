use gw_task::{
    template::{eval_json_path, TemplateError},
    ClientMode,
};
use reqwest::header::HeaderMap;
use serde_json::Value;

pub fn select_client_mode(
    profile: &Value,
    _method: &str,
    _path: &str,
    headers: &HeaderMap,
    query: &str,
    body: Option<&Value>,
) -> Result<ClientMode, TemplateError> {
    let enabled = enabled_modes(profile);
    if enabled.is_empty() {
        return Err(TemplateError::Invalid(
            "at least one client mode is required".to_owned(),
        ));
    }

    if let Some(mode) = select_by_rules(profile, headers, query, body)? {
        return parse_enabled_mode(&mode, &enabled);
    }

    if enabled.len() == 1 {
        return parse_enabled_mode(&enabled[0], &enabled);
    }

    if prefer_respond_async(headers) {
        return parse_enabled_mode("async", &enabled);
    }

    let default = profile
        .get("default_client_mode")
        .and_then(Value::as_str)
        .unwrap_or("sync");
    parse_enabled_mode(default, &enabled)
}

fn enabled_modes(profile: &Value) -> Vec<String> {
    profile
        .get("client_modes")
        .and_then(Value::as_array)
        .map(|modes| {
            modes
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn select_by_rules(
    profile: &Value,
    headers: &HeaderMap,
    query: &str,
    body: Option<&Value>,
) -> Result<Option<String>, TemplateError> {
    let Some(rules) = profile
        .get("client_mode_selector")
        .and_then(|selector| selector.get("rules"))
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };

    for rule in rules {
        let Some(rule) = rule.as_object() else {
            continue;
        };
        let source = rule
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let expected = rule.get("equals").unwrap_or(&Value::Null);
        let actual = match source {
            "header" => rule
                .get("name")
                .and_then(Value::as_str)
                .and_then(|name| headers.get(name))
                .and_then(|value| value.to_str().ok())
                .map(|value| Value::String(value.to_owned())),
            "query" => rule
                .get("name")
                .and_then(Value::as_str)
                .and_then(|name| query_param(query, name))
                .map(Value::String),
            "body" => rule
                .get("path")
                .and_then(Value::as_str)
                .and_then(|path| body.and_then(|body| eval_json_path(body, path).ok())),
            _ => None,
        };

        if actual.as_ref().is_some_and(|value| value == expected) {
            return Ok(rule
                .get("mode")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned));
        }
    }
    Ok(None)
}

fn parse_enabled_mode(mode: &str, enabled: &[String]) -> Result<ClientMode, TemplateError> {
    if !enabled.iter().any(|candidate| candidate == mode) {
        return Err(TemplateError::ClientModeUnsupported(format!(
            "client mode {mode} is not enabled"
        )));
    }
    match mode {
        "sync" => Ok(ClientMode::Sync),
        "async" => Ok(ClientMode::Async),
        other => Err(TemplateError::ClientModeUnsupported(format!(
            "unsupported client mode {other}"
        ))),
    }
}

fn prefer_respond_async(headers: &HeaderMap) -> bool {
    headers
        .get("prefer")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("respond-async"))
        })
}

fn query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == name {
            return Some(value.to_owned());
        }
    }
    None
}
