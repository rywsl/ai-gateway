//! ADR-023 five-tier permission merge:
//! global > user-group > user (main account) > sub-account > api-key.
//!
//! - `allowed_models` / `allowed_channels` / `allowed_groups` use **intersection**
//!   with the convention that an *empty* set at a tier means "no restriction at
//!   that tier" and is therefore skipped.
//! - `rpm_limit` / `tpm_limit` take the **minimum positive** across all tiers.
//! - `markup` is the **product** across all tiers (defaults to 1.0).
//! - `forced_model_group` comes from the highest-priority user group (if any).
//! - `effective_user_id` is the parent main-account id for a sub-account,
//!   otherwise the user's own id.

use gw_core::ids::*;
use gw_core::permissions::MergedPermissions;
use gw_store::EntityStore;
use std::collections::HashSet;

use crate::AuthCtx;

pub async fn merge_permissions(
    ctx: &AuthCtx,
    store: &EntityStore,
    global_rpm: u32,
) -> anyhow::Result<MergedPermissions> {
    let user = store
        .user
        .get(&ctx.user_id)
        .await
        .ok_or_else(|| anyhow::anyhow!("user not found"))?;

    let main = match &user.value.parent_id {
        Some(p) => store
            .user
            .get(p)
            .await
            .ok_or_else(|| anyhow::anyhow!("parent not found"))?,
        None => user.clone(),
    };

    let mut groups = Vec::new();
    for gid in &main.value.user_group_ids {
        if let Some(g) = store.user_group.get(gid).await {
            groups.push(g);
        }
    }

    let api_key = if let Some(akid) = &ctx.api_key_id {
        store.apikey.get(akid).await
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
    // forced model_group: highest-priority group's binding
    // ---------------------------------------------------------------------
    let forced = groups
        .iter()
        .max_by_key(|g| g.value.priority)
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
        markup_product: markup,
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
