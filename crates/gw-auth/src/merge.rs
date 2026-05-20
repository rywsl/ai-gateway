//! ADR-023 five-tier permission merge:
//! global > user-group > user (main account) > sub-account > api-key.
//!
//! - `allowed_models` / `allowed_channels` / `allowed_groups` use **intersection**
//!   with the convention that an *empty* set at a tier means "no restriction at
//!   that tier" and is therefore skipped.
//! - `rpm_limit` / `tpm_limit` take the **minimum positive** across all tiers.
//! - `markup` is the **product** across all tiers (defaults to 1.0).
//! - `forced_model_group` comes from the highest-priority user group, with ties
//!   broken deterministically by lexicographic `UserGroupId`.
//! - `effective_user_id` is the parent main-account id for a sub-account,
//!   otherwise the user's own id.

use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::*;
use gw_core::permissions::MergedPermissions;
use gw_store::EntityStore;
use std::borrow::Cow;
use std::collections::HashSet;

pub use gw_core::permissions::MarkupChainSnapshot;

use crate::AuthCtx;

pub async fn merge_permissions(
    ctx: &AuthCtx,
    store: &EntityStore,
    global_rpm: u32,
) -> Result<MergedPermissions, ErrorIR<'static>> {
    let user = store
        .user_or_load(&ctx.user_id)
        .await
        .ok_or_else(user_not_found)?;

    let main = match &user.value.parent_id {
        Some(p) => store.user_or_load(p).await.ok_or_else(user_not_found)?,
        None => user.clone(),
    };

    let mut groups = Vec::new();
    for gid in &main.value.user_group_ids {
        if let Some(g) = store.user_group_or_load(gid).await {
            groups.push(g);
        }
    }

    let api_key = if let Some(akid) = &ctx.api_key_id {
        store.apikey_or_load(akid).await
    } else {
        None
    };

    // ---------------------------------------------------------------------
    // allowed_models intersection (empty = unrestricted at that tier)
    // ---------------------------------------------------------------------
    let mut allowed_models: HashSet<ModelName> = user.value.allowed_models.iter().cloned().collect();
    intersect(&mut allowed_models, main.value.allowed_models.iter());
    for g in &groups {
        intersect(&mut allowed_models, g.value.allowed_models.iter());
    }
    if let Some(ak) = &api_key {
        intersect(&mut allowed_models, ak.value.allowed_models.iter());
    }

    // ---------------------------------------------------------------------
    // allowed_channels intersection
    // ---------------------------------------------------------------------
    let mut allowed_channels: HashSet<KeyId> =
        user.value.allowed_channels.iter().cloned().collect();
    intersect(&mut allowed_channels, main.value.allowed_channels.iter());
    for g in &groups {
        intersect(&mut allowed_channels, g.value.allowed_channels.iter());
    }
    if let Some(ak) = &api_key {
        intersect(&mut allowed_channels, ak.value.allowed_channels.iter());
    }

    // ---------------------------------------------------------------------
    // allowed_groups intersection (sub-account inherits from main only via UserEntry.allowed_groups)
    // ---------------------------------------------------------------------
    let mut allowed_groups: HashSet<Group> =
        user.value.allowed_groups.iter().cloned().collect();
    intersect(&mut allowed_groups, main.value.allowed_groups.iter());

    // ---------------------------------------------------------------------
    // RPM / TPM = min positive
    // ---------------------------------------------------------------------
    let groups_min_rpm = groups.iter().map(|g| g.value.rpm_limit).min().unwrap_or(0);
    let groups_min_tpm = groups.iter().map(|g| g.value.tpm_limit).min().unwrap_or(0);
    let rpm = min_pos(&[
        user.value.rpm_limit,
        main.value.rpm_limit,
        groups_min_rpm,
        api_key.as_ref().map(|a| a.value.rpm_limit).unwrap_or(0),
        global_rpm,
    ]);
    let tpm = min_pos(&[
        user.value.tpm_limit,
        main.value.tpm_limit,
        groups_min_tpm,
        api_key.as_ref().map(|a| a.value.tpm_limit).unwrap_or(0),
    ]);

    // ---------------------------------------------------------------------
    // markup product
    // ---------------------------------------------------------------------
    let markup = user.value.markup
        * main.value.markup
        * groups.iter().fold(1.0_f32, |a, g| a * g.value.markup)
        * api_key.as_ref().map(|a| a.value.markup).unwrap_or(1.0);

    // ---------------------------------------------------------------------
    // forced model_group: highest-priority group's binding.
    //
    // Sort by (descending priority, ascending UserGroupId) so ties resolve
    // deterministically — `max_by_key` would otherwise pick the *last* element
    // with the max priority, which depends on `user_group_ids` insertion order.
    // ---------------------------------------------------------------------
    let mut groups_sorted: Vec<_> = groups.iter().collect();
    groups_sorted.sort_by(|a, b| {
        b.value
            .priority
            .cmp(&a.value.priority)
            .then_with(|| a.value.id.as_ref().cmp(b.value.id.as_ref()))
    });
    let forced = groups_sorted
        .first()
        .and_then(|g| g.value.model_group_binding.clone());

    // ---------------------------------------------------------------------
    // effective_user_id: sub-account -> parent
    // ---------------------------------------------------------------------
    let effective = user
        .value
        .parent_id
        .clone()
        .unwrap_or_else(|| ctx.user_id.clone());

    Ok(MergedPermissions {
        user_id: ctx.user_id.clone(),
        effective_user_id: effective,
        client_ip: ctx.client_ip,
        allowed_models,
        allowed_channels,
        allowed_groups,
        forced_model_group: forced,
        rpm_limit: rpm,
        tpm_limit: tpm,
        max_concurrent_streams: main.value.max_concurrent_streams,
        markup_chain: MarkupChainSnapshot {
            // Phase 1b: per-tier values left as 1.0 placeholders.
            // Phase 5 (Bm1) populates these from the actual entity reads above.
            user_group_markup: 1.0,
            api_key_markup: 1.0,
            vendor_markup: 1.0,
            model_markup: 1.0,
            product: markup,
        },
    })
}

/// Intersect `set` with `other` *only when* `other` is non-empty (empty == unrestricted).
fn intersect<'a, T, I>(set: &mut HashSet<T>, other: I)
where
    T: Eq + std::hash::Hash + Clone + 'a,
    I: IntoIterator<Item = &'a T>,
{
    let o: HashSet<T> = other.into_iter().cloned().collect();
    if !o.is_empty() {
        if set.is_empty() {
            // Empty on this side means "no restriction yet" — adopt the other tier wholesale.
            *set = o;
        } else {
            set.retain(|m| o.contains(m));
        }
    }
}

fn min_pos(vals: &[u32]) -> u32 {
    vals.iter().filter(|v| **v > 0).min().copied().unwrap_or(0)
}

/// Missing-user / missing-parent error.
///
/// No dedicated `GW_AUTH_USER_NOT_FOUND` code exists yet; we reuse
/// `GW_AUTH_INVALID_APIKEY` (auth context fails closed at the same 401 status)
/// to keep the stable-codes dictionary stable and avoid cross-crate churn.
fn user_not_found() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientAuth,
        status: 401,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_AUTH_INVALID_APIKEY),
        message: Cow::Borrowed("user not found"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}
