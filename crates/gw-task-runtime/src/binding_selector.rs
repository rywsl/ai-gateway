use gw_core::entity::{AsyncTaskBindingIndex, AsyncTaskBindingRevision};
use gw_task::template::{TemplateError, BUILTIN_MODEL_ID_ONLY};
use serde_json::Value;

pub const BUILTIN_MODEL_ID_ONLY_PROFILE_ID: &str = "atp_builtin_model_id_only";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedTarget {
    pub task_kind: String,
    pub global_model_id: String,
    pub provider_model_id: String,
    pub endpoint_id: String,
    pub api_format: String,
    pub provider_key_id: Option<String>,
    pub openai_compatible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BindingSelection {
    Published(AsyncTaskBindingRevision),
    Builtin {
        profile: Value,
        binding: Value,
        effective_template_hash: String,
    },
}

pub fn select_binding(
    index: &AsyncTaskBindingIndex,
    revisions: &[AsyncTaskBindingRevision],
    target: &SelectedTarget,
) -> Result<BindingSelection, TemplateError> {
    let key_mismatch_only = revisions
        .iter()
        .filter(|revision| revision_matches_index(index, revision))
        .any(|revision| revision_matches_target_except_key(revision, target))
        && !revisions
            .iter()
            .filter(|revision| revision_matches_index(index, revision))
            .any(|revision| revision_matches_target(revision, target));

    let mut matches = revisions
        .iter()
        .filter(|revision| revision_matches_index(index, revision))
        .filter(|revision| revision_matches_target(revision, target))
        .map(|revision| (specificity_rank(revision), revision.priority, revision))
        .collect::<Vec<_>>();

    matches.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| left.2.id.cmp(&right.2.id))
    });

    if let Some((_, _, revision)) = matches.into_iter().next() {
        return Ok(BindingSelection::Published(revision.clone()));
    }

    if key_mismatch_only {
        return Err(TemplateError::ProfileNotConfigured(format!(
            "async task binding provider key does not match selected key for {}/{}",
            target.global_model_id, target.task_kind
        )));
    }

    if target.openai_compatible && matches_builtin_kind(&target.task_kind) {
        return builtin_model_id_only();
    }

    Err(TemplateError::ProfileNotConfigured(format!(
        "async task profile is not configured for {}/{}",
        target.global_model_id, target.task_kind
    )))
}

fn revision_matches_index(
    index: &AsyncTaskBindingIndex,
    revision: &AsyncTaskBindingRevision,
) -> bool {
    (index.global_model_id.is_empty() || index.global_model_id == revision.global_model_id)
        && (index.task_kind.is_empty() || index.task_kind == revision.task_kind)
}

fn revision_matches_target(revision: &AsyncTaskBindingRevision, target: &SelectedTarget) -> bool {
    revision_matches_target_except_key(revision, target)
        && provider_key_matches(
            revision.provider_key_id.as_str(),
            target.provider_key_id.as_deref(),
        )
}

fn revision_matches_target_except_key(
    revision: &AsyncTaskBindingRevision,
    target: &SelectedTarget,
) -> bool {
    field_matches(&revision.task_kind, &target.task_kind)
        && field_matches(&revision.global_model_id, &target.global_model_id)
        && field_matches(&revision.provider_model_id, &target.provider_model_id)
        && field_matches(&revision.endpoint_id, &target.endpoint_id)
        && field_matches(&revision.api_format, &target.api_format)
}

fn field_matches(binding_value: &str, target_value: &str) -> bool {
    binding_value.is_empty() || binding_value == target_value
}

fn provider_key_matches(binding_value: &str, selected_key: Option<&str>) -> bool {
    binding_value.is_empty() || Some(binding_value) == selected_key
}

fn specificity_rank(revision: &AsyncTaskBindingRevision) -> u8 {
    [
        revision.task_kind.as_str(),
        revision.global_model_id.as_str(),
        revision.provider_model_id.as_str(),
        revision.endpoint_id.as_str(),
        revision.api_format.as_str(),
        revision.provider_key_id.as_str(),
    ]
    .iter()
    .filter(|value| !value.is_empty())
    .count() as u8
}

fn matches_builtin_kind(task_kind: &str) -> bool {
    matches!(task_kind, "image" | "embedding")
}

fn builtin_model_id_only() -> Result<BindingSelection, TemplateError> {
    let fixture: Value = serde_json::from_str(BUILTIN_MODEL_ID_ONLY).map_err(|err| {
        TemplateError::Invalid(format!("decode built-in model_id_only fixture: {err}"))
    })?;
    let profile = fixture
        .get("profile")
        .cloned()
        .ok_or_else(|| TemplateError::Invalid("built-in profile is missing".to_owned()))?;
    let binding = fixture
        .get("binding")
        .cloned()
        .ok_or_else(|| TemplateError::Invalid("built-in binding is missing".to_owned()))?;
    let effective_template_hash = gw_task::template::hash_effective_template(&serde_json::json!({
        "profile": profile,
        "binding": binding,
    }));

    Ok(BindingSelection::Builtin {
        profile,
        binding,
        effective_template_hash,
    })
}
