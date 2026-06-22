use gw_core::entity::{AsyncTaskBindingIndex, AsyncTaskBindingRevision};
use gw_task::template::TemplateError;
use gw_task_runtime::binding_selector::{
    select_binding, BindingSelection, SelectedTarget, BUILTIN_MODEL_ID_ONLY_PROFILE_ID,
};

fn rev(
    id: &str,
    task_kind: &str,
    global_model_id: &str,
    provider_model_id: &str,
    endpoint_id: &str,
    api_format: &str,
    provider_key_id: &str,
    priority: i64,
) -> AsyncTaskBindingRevision {
    AsyncTaskBindingRevision {
        id: id.to_owned(),
        binding_id: id.replace(":v1", ""),
        version: 1,
        profile_id: format!("profile_{id}"),
        profile_revision_id: format!("profile_{id}:v1"),
        profile_version: 1,
        task_kind: task_kind.to_owned(),
        global_model_id: global_model_id.to_owned(),
        provider_id: "provider_openai".to_owned(),
        provider_model_id: provider_model_id.to_owned(),
        endpoint_id: endpoint_id.to_owned(),
        api_format: api_format.to_owned(),
        provider_key_id: provider_key_id.to_owned(),
        priority,
        target_snapshot: serde_json::json!({}),
        overrides_snapshot: serde_json::json!({}),
        effective_template_snapshot: serde_json::json!({"id": format!("profile_{id}")}),
        effective_template_hash: format!("sha256:{id}"),
        specificity_key: format!(
            "{}|{}|{}|{}|{}|{}",
            if task_kind.is_empty() { "*" } else { task_kind },
            if global_model_id.is_empty() {
                "*"
            } else {
                global_model_id
            },
            provider_model_id,
            if endpoint_id.is_empty() {
                "*"
            } else {
                endpoint_id
            },
            if api_format.is_empty() {
                "*"
            } else {
                api_format
            },
            if provider_key_id.is_empty() {
                "*"
            } else {
                provider_key_id
            },
        ),
        published_by: "admin".to_owned(),
        published_at: 1,
    }
}

fn target() -> SelectedTarget {
    SelectedTarget {
        task_kind: "image".to_owned(),
        global_model_id: "gpt-image".to_owned(),
        provider_model_id: "pm_image".to_owned(),
        endpoint_id: "endpoint_a".to_owned(),
        api_format: "openai".to_owned(),
        provider_key_id: Some("key_a".to_owned()),
        openai_compatible: true,
    }
}

fn published_revision_id(selection: &BindingSelection) -> &str {
    match selection {
        BindingSelection::Published(revision) => revision.id.as_str(),
        BindingSelection::Builtin { .. } => panic!("expected published binding"),
    }
}

#[test]
fn selects_most_specific_binding_before_priority() {
    let index = AsyncTaskBindingIndex {
        global_model_id: "gpt-image".to_owned(),
        task_kind: "image".to_owned(),
        binding_revision_ids: vec![],
        endpoint_registry: vec![],
    };
    let revisions = vec![
        rev(
            "generic:v1",
            "image",
            "gpt-image",
            "pm_image",
            "",
            "openai",
            "",
            999,
        ),
        rev(
            "endpoint:v1",
            "image",
            "gpt-image",
            "pm_image",
            "endpoint_a",
            "openai",
            "",
            1,
        ),
        rev(
            "key:v1",
            "image",
            "gpt-image",
            "pm_image",
            "endpoint_a",
            "openai",
            "key_a",
            1,
        ),
    ];

    let selected = select_binding(&index, &revisions, &target()).expect("binding");

    assert_eq!(published_revision_id(&selected), "key:v1");
}

#[test]
fn sorts_by_priority_within_the_same_specificity_level() {
    let index = AsyncTaskBindingIndex {
        global_model_id: "gpt-image".to_owned(),
        task_kind: "image".to_owned(),
        binding_revision_ids: vec![],
        endpoint_registry: vec![],
    };
    let revisions = vec![
        rev(
            "low:v1",
            "image",
            "gpt-image",
            "pm_image",
            "endpoint_a",
            "openai",
            "",
            5,
        ),
        rev(
            "high:v1",
            "image",
            "gpt-image",
            "pm_image",
            "endpoint_a",
            "openai",
            "",
            10,
        ),
    ];

    let selected = select_binding(&index, &revisions, &target()).expect("binding");

    assert_eq!(published_revision_id(&selected), "high:v1");
}

#[test]
fn rejects_provider_key_specific_binding_when_selected_key_differs() {
    let index = AsyncTaskBindingIndex {
        global_model_id: "gpt-image".to_owned(),
        task_kind: "image".to_owned(),
        binding_revision_ids: vec![],
        endpoint_registry: vec![],
    };
    let revisions = vec![rev(
        "wrong_key:v1",
        "image",
        "gpt-image",
        "pm_image",
        "endpoint_a",
        "openai",
        "key_b",
        100,
    )];

    let err = select_binding(&index, &revisions, &target()).expect_err("key mismatch");

    assert!(matches!(err, TemplateError::ProfileNotConfigured(_)));
    assert_eq!(err.code(), "GW_TASK_PROFILE_NOT_CONFIGURED");
}

#[test]
fn falls_back_to_builtin_model_id_only_for_openai_compatible_image() {
    let index = AsyncTaskBindingIndex {
        global_model_id: "gpt-image".to_owned(),
        task_kind: "image".to_owned(),
        binding_revision_ids: vec![],
        endpoint_registry: vec![],
    };

    let selected = select_binding(&index, &[], &target()).expect("builtin fallback");

    match selected {
        BindingSelection::Published(_) => panic!("expected builtin fallback"),
        BindingSelection::Builtin { profile, .. } => {
            assert_eq!(profile["id"], BUILTIN_MODEL_ID_ONLY_PROFILE_ID);
        }
    }
}

#[test]
fn non_openai_compatible_targets_fail_closed_when_unconfigured() {
    let index = AsyncTaskBindingIndex {
        global_model_id: "veo".to_owned(),
        task_kind: "video".to_owned(),
        binding_revision_ids: vec![],
        endpoint_registry: vec![],
    };
    let non_compatible = SelectedTarget {
        task_kind: "video".to_owned(),
        global_model_id: "veo".to_owned(),
        provider_model_id: "pm_veo".to_owned(),
        endpoint_id: "google_lro".to_owned(),
        api_format: "gemini".to_owned(),
        provider_key_id: None,
        openai_compatible: false,
    };

    let err = select_binding(&index, &[], &non_compatible).expect_err("unconfigured target");

    assert_eq!(err.code(), "GW_TASK_PROFILE_NOT_CONFIGURED");
}
