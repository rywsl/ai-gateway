use gw_task::{template::TemplateError, ClientMode};
use gw_task_runtime::client_mode::select_client_mode;
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::json;

#[test]
fn single_mode_profiles_return_their_enabled_mode() {
    let sync_profile = json!({
        "client_modes": ["sync"],
        "default_client_mode": "sync"
    });
    let async_profile = json!({
        "client_modes": ["async"],
        "default_client_mode": "async"
    });

    assert_eq!(
        select_client_mode(
            &sync_profile,
            "POST",
            "/v1/images/generations",
            &HeaderMap::new(),
            "",
            None
        )
        .expect("sync mode"),
        ClientMode::Sync
    );
    assert_eq!(
        select_client_mode(
            &async_profile,
            "POST",
            "/v1/images/generations",
            &HeaderMap::new(),
            "",
            None
        )
        .expect("async mode"),
        ClientMode::Async
    );
}

#[test]
fn prefer_respond_async_selects_async_when_both_modes_are_enabled() {
    let profile = json!({
        "client_modes": ["sync", "async"],
        "default_client_mode": "sync"
    });
    let mut headers = HeaderMap::new();
    headers.insert("prefer", HeaderValue::from_static("respond-async"));

    let mode = select_client_mode(
        &profile,
        "POST",
        "/v1/images/generations",
        &headers,
        "",
        None,
    )
    .expect("client mode");

    assert_eq!(mode, ClientMode::Async);
}

#[test]
fn selector_hit_for_disabled_mode_returns_stable_error_code() {
    let profile = json!({
        "client_modes": ["sync"],
        "default_client_mode": "sync",
        "client_mode_selector": {
            "rules": [{"source": "query", "name": "async", "equals": "true", "mode": "async"}]
        }
    });

    let err = select_client_mode(
        &profile,
        "POST",
        "/v1/images/generations",
        &HeaderMap::new(),
        "async=true",
        None,
    )
    .expect_err("async mode is disabled");

    assert!(matches!(err, TemplateError::ClientModeUnsupported(_)));
    assert_eq!(err.code(), "GW_TASK_CLIENT_MODE_UNSUPPORTED");
}

#[test]
fn selector_miss_uses_default_client_mode() {
    let profile = json!({
        "client_modes": ["sync", "async"],
        "default_client_mode": "sync",
        "client_mode_selector": {
            "rules": [
                {"source": "body", "path": "$.background", "equals": true, "mode": "async"}
            ]
        }
    });
    let body = json!({"background": false});

    let mode = select_client_mode(
        &profile,
        "POST",
        "/v1/images/generations",
        &HeaderMap::new(),
        "",
        Some(&body),
    )
    .expect("client mode");

    assert_eq!(mode, ClientMode::Sync);
}

#[test]
fn body_boolean_rule_preserves_json_boolean_comparison() {
    let profile = json!({
        "client_modes": ["sync", "async"],
        "default_client_mode": "sync",
        "client_mode_selector": {
            "rules": [
                {"source": "body", "path": "$.background", "equals": true, "mode": "async"}
            ]
        }
    });
    let body = json!({"background": true});

    let mode = select_client_mode(
        &profile,
        "POST",
        "/v1/images/generations",
        &HeaderMap::new(),
        "",
        Some(&body),
    )
    .expect("client mode");

    assert_eq!(mode, ClientMode::Async);
}
