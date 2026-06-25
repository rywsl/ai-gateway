use std::sync::Arc;

use gw_core::entity::{AsyncTaskBindingIndex, AsyncTaskBindingRevision, AsyncTaskProfileRevision};
use gw_core::versioned::Versioned;
use gw_proxy::filters::task::{resolve_execution_modes, resolve_task_runtime, TaskRequestKey};
use gw_store::EntityStore;
use reqwest::header::{HeaderMap, HeaderValue};

fn versioned<T>(value: T) -> Arc<Versioned<T>> {
    Arc::new(Versioned::new(1, value))
}

fn binding_revision(
    id: &str,
    profile_revision_id: &str,
    task_kind: &str,
    global_model_id: &str,
    priority: i64,
) -> AsyncTaskBindingRevision {
    binding_revision_with_target(
        id,
        profile_revision_id,
        task_kind,
        global_model_id,
        priority,
        serde_json::json!({}),
    )
}

fn binding_revision_with_target(
    id: &str,
    profile_revision_id: &str,
    task_kind: &str,
    global_model_id: &str,
    priority: i64,
    target_snapshot: serde_json::Value,
) -> AsyncTaskBindingRevision {
    AsyncTaskBindingRevision {
        id: id.to_string(),
        binding_id: id.replace(":v1", ""),
        version: 1,
        profile_id: "profile-openai-image".to_string(),
        profile_revision_id: profile_revision_id.to_string(),
        profile_version: 1,
        task_kind: task_kind.to_string(),
        global_model_id: global_model_id.to_string(),
        provider_id: "openai".to_string(),
        provider_model_id: global_model_id.to_string(),
        endpoint_id: "endpoint-openai".to_string(),
        api_format: "openai".to_string(),
        provider_key_id: String::new(),
        priority,
        target_snapshot,
        overrides_snapshot: serde_json::json!({}),
        effective_template_snapshot: serde_json::json!({
            "id": "profile-openai-image",
            "task_kind": task_kind,
            "client_modes": ["sync"],
            "default_client_mode": "sync",
            "upstream_mode": "sync"
        }),
        effective_template_hash: format!("sha256:{id}"),
        specificity_key: format!("{task_kind}|{global_model_id}|*|*|openai|*"),
        published_by: "admin".to_string(),
        published_at: 1,
    }
}

fn profile_revision(id: &str, task_kind: &str) -> AsyncTaskProfileRevision {
    AsyncTaskProfileRevision {
        id: id.to_string(),
        profile_id: "profile-openai-image".to_string(),
        version: 1,
        schema_version: "async-task-template.v1".to_string(),
        task_kind: task_kind.to_string(),
        profile: serde_json::json!({
            "id": "profile-openai-image",
            "task_kind": task_kind,
            "client_modes": ["sync"],
            "default_client_mode": "sync",
            "upstream_mode": "sync"
        }),
        template_hash: format!("sha256:{id}"),
        published_by: "admin".to_string(),
        published_at: 1,
    }
}

fn profile_revision_with_endpoint(
    id: &str,
    task_kind: &str,
    method: &str,
    path: &str,
) -> AsyncTaskProfileRevision {
    AsyncTaskProfileRevision {
        id: id.to_string(),
        profile_id: id.replace(":v1", ""),
        version: 1,
        schema_version: "async-task-template.v1".to_string(),
        task_kind: task_kind.to_string(),
        profile: serde_json::json!({
            "id": id.replace(":v1", ""),
            "task_kind": task_kind,
            "client_modes": ["sync"],
            "default_client_mode": "sync",
            "upstream_mode": "sync",
            "client_request_template": {
                "endpoint": {
                    "method": method,
                    "path": path,
                    "task_kind": task_kind,
                    "model_path": "$.model"
                }
            }
        }),
        template_hash: format!("sha256:{id}"),
        published_by: "admin".to_string(),
        published_at: 1,
    }
}

#[tokio::test]
async fn resolves_published_runtime_from_binding_index() {
    let store = Arc::new(EntityStore::new());
    store
        .async_task_binding_index
        .insert(
            "gpt-image-2:image".to_string(),
            versioned(AsyncTaskBindingIndex {
                global_model_id: "gpt-image-2".to_string(),
                task_kind: "image".to_string(),
                binding_revision_ids: vec!["binding-image:v1".to_string()],
                endpoint_registry: vec![],
            }),
        )
        .await;
    store
        .async_task_binding_revision
        .insert(
            "binding-image:v1".to_string(),
            versioned(binding_revision(
                "binding-image:v1",
                "profile-image:v1",
                "image",
                "gpt-image-2",
                10,
            )),
        )
        .await;
    store
        .async_task_profile_revision
        .insert(
            "profile-image:v1".to_string(),
            versioned(profile_revision("profile-image:v1", "image")),
        )
        .await;

    let runtime = resolve_task_runtime(&store, &TaskRequestKey::new("gpt-image-2", "image"))
        .await
        .expect("runtime");

    assert_eq!(runtime.profile_revision_id, "profile-image:v1");
    assert_eq!(runtime.binding_revision_id, "binding-image:v1");
    assert_eq!(runtime.profile["task_kind"], "image");
    assert_eq!(runtime.binding["global_model_id"], "gpt-image-2");
    assert_eq!(runtime.upstream_base_url, None);
}

#[tokio::test]
async fn resolves_upstream_base_url_from_target_snapshot() {
    let store = Arc::new(EntityStore::new());
    store
        .async_task_binding_index
        .insert(
            "gpt-image-2:image".to_string(),
            versioned(AsyncTaskBindingIndex {
                global_model_id: "gpt-image-2".to_string(),
                task_kind: "image".to_string(),
                binding_revision_ids: vec!["binding-image:v1".to_string()],
                endpoint_registry: vec![],
            }),
        )
        .await;
    store
        .async_task_binding_revision
        .insert(
            "binding-image:v1".to_string(),
            versioned(binding_revision_with_target(
                "binding-image:v1",
                "profile-image:v1",
                "image",
                "gpt-image-2",
                10,
                serde_json::json!({"base_url": "https://api.example.test"}),
            )),
        )
        .await;
    store
        .async_task_profile_revision
        .insert(
            "profile-image:v1".to_string(),
            versioned(profile_revision("profile-image:v1", "image")),
        )
        .await;

    let runtime = resolve_task_runtime(&store, &TaskRequestKey::new("gpt-image-2", "image"))
        .await
        .expect("runtime");

    assert_eq!(
        runtime.upstream_base_url.as_deref(),
        Some("https://api.example.test")
    );
}

#[tokio::test]
async fn resolves_highest_priority_published_runtime() {
    let store = Arc::new(EntityStore::new());
    store
        .async_task_binding_index
        .insert(
            "text-embedding-3-large:embedding".to_string(),
            versioned(AsyncTaskBindingIndex {
                global_model_id: "text-embedding-3-large".to_string(),
                task_kind: "embedding".to_string(),
                binding_revision_ids: vec![
                    "binding-low:v1".to_string(),
                    "binding-high:v1".to_string(),
                ],
                endpoint_registry: vec![],
            }),
        )
        .await;
    for (id, priority) in [("binding-low:v1", 1), ("binding-high:v1", 100)] {
        store
            .async_task_binding_revision
            .insert(
                id.to_string(),
                versioned(binding_revision(
                    id,
                    "profile-embedding:v1",
                    "embedding",
                    "text-embedding-3-large",
                    priority,
                )),
            )
            .await;
    }
    store
        .async_task_profile_revision
        .insert(
            "profile-embedding:v1".to_string(),
            versioned(profile_revision("profile-embedding:v1", "embedding")),
        )
        .await;

    let runtime = resolve_task_runtime(
        &store,
        &TaskRequestKey::new("text-embedding-3-large", "embedding"),
    )
    .await
    .expect("runtime");

    assert_eq!(runtime.binding_revision_id, "binding-high:v1");
}

#[tokio::test]
async fn resolves_published_runtime_matching_openai_compatible_endpoint_route() {
    let store = Arc::new(EntityStore::new());
    store
        .async_task_binding_index
        .insert(
            "sora-2:video".to_string(),
            versioned(AsyncTaskBindingIndex {
                global_model_id: "sora-2".to_string(),
                task_kind: "video".to_string(),
                binding_revision_ids: vec![
                    "binding-video-create:v1".to_string(),
                    "binding-video-edit:v1".to_string(),
                ],
                endpoint_registry: vec![],
            }),
        )
        .await;
    for (id, profile_id, priority) in [
        ("binding-video-create:v1", "profile-video-create:v1", 10),
        ("binding-video-edit:v1", "profile-video-edit:v1", 1),
    ] {
        store
            .async_task_binding_revision
            .insert(
                id.to_string(),
                versioned(binding_revision(
                    id, profile_id, "video", "sora-2", priority,
                )),
            )
            .await;
    }
    store
        .async_task_profile_revision
        .insert(
            "profile-video-create:v1".to_string(),
            versioned(profile_revision_with_endpoint(
                "profile-video-create:v1",
                "video",
                "POST",
                "/v1/videos",
            )),
        )
        .await;
    store
        .async_task_profile_revision
        .insert(
            "profile-video-edit:v1".to_string(),
            versioned(profile_revision_with_endpoint(
                "profile-video-edit:v1",
                "video",
                "POST",
                "/v1/videos/edits",
            )),
        )
        .await;

    let runtime = resolve_task_runtime(
        &store,
        &TaskRequestKey {
            global_model_id: "sora-2".to_string(),
            task_kind: "video".to_string(),
            endpoint_method: Some("POST".to_string()),
            endpoint_path: Some("/v1/videos/edits".to_string()),
        },
    )
    .await
    .expect("runtime");

    assert_eq!(runtime.profile_revision_id, "profile-video-edit:v1");
    assert_eq!(runtime.binding_revision_id, "binding-video-edit:v1");
}

#[tokio::test]
async fn missing_binding_index_returns_profile_not_configured() {
    let store = Arc::new(EntityStore::new());

    let err = resolve_task_runtime(&store, &TaskRequestKey::new("missing-model", "video"))
        .await
        .expect_err("missing runtime");

    assert_eq!(err.status, 404);
    assert_eq!(err.code, "GW_TASK_PROFILE_NOT_CONFIGURED");
}

#[test]
fn resolves_execution_modes_from_profile_and_request_headers() {
    let profile = serde_json::json!({
        "client_modes": ["sync", "async"],
        "default_client_mode": "sync",
        "upstream_mode": "async_lro"
    });
    let mut headers = HeaderMap::new();
    headers.insert("prefer", HeaderValue::from_static("respond-async"));

    let modes = resolve_execution_modes(
        &profile,
        "POST",
        "/v1/videos",
        &headers,
        "",
        Some(&serde_json::json!({"model": "sora-2"})),
    )
    .expect("modes");

    assert_eq!(modes.client_mode, gw_task::ClientMode::Async);
    assert_eq!(modes.upstream_mode, gw_task::UpstreamMode::AsyncLro);
}

#[test]
fn resolve_execution_modes_rejects_disabled_selected_mode() {
    let profile = serde_json::json!({
        "client_modes": ["sync"],
        "default_client_mode": "sync",
        "upstream_mode": "sync",
        "client_mode_selector": {
            "rules": [{"source": "query", "name": "async", "equals": "true", "mode": "async"}]
        }
    });

    let err = resolve_execution_modes(
        &profile,
        "POST",
        "/v1/images/generations",
        &HeaderMap::new(),
        "async=true",
        Some(&serde_json::json!({"model": "gpt-image-2"})),
    )
    .expect_err("disabled async");

    assert_eq!(err.status, 400);
    assert_eq!(err.code, "GW_TASK_CLIENT_MODE_UNSUPPORTED");
}
