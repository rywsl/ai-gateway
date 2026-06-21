//! Candidate key filter (ADR-021).
//!
//! Given a `ModelGroup` and request context, walk every member `KeyId`, fetch
//! its `VendorKey` from the store, and admit it only if:
//! 1. `status == Active`,
//! 2. the requested `model` is in `accessible_models`,
//! 3. the target wire protocol is in `accessible_endpoints` OR the key has
//!    `protocol_conversion = true`,
//! 4. the caller's `MergedPermissions.allowed_channels` is either empty (no
//!    channel allowlist) or contains this key,
//! 5. the key's in-flight count is under the per-key cap,
//! 6. the `key_id` is NOT in `retry_blacklist` (G10) — i.e. already attempted
//!    and failed earlier in this request.
//! 7. (G13) the channel's advertised `forced_capabilities` satisfy the
//!    request's `RequiredCapabilities` per `gw_capability::candidate_matches`
//!    (a channel with no advertised caps is non-restrictive; `caps == None`
//!    disables the filter entirely).
//!
//! Health/circuit checks happen *later* inside `Strategy::pick` via
//! `KeyHealthStore::is_available` — candidate filtering is the cheap
//! upstream step.

use crate::in_flight::InFlightStore;
use gw_capability::{candidate_matches, parse_capabilities, RequiredCapabilities};
use gw_core::entity::{KeyStatus, ModelGroup, VendorKey};
use gw_core::ids::{KeyId, ModelName};
use gw_core::permissions::MergedPermissions;
use gw_core::protocol::ProtocolKind;
use gw_store::entity_store::EntityStore;
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;

#[allow(clippy::too_many_arguments)]
pub async fn get_candidate_keys(
    group: &ModelGroup,
    model: &ModelName,
    target_protocol: ProtocolKind,
    perm: &MergedPermissions,
    in_flight: &InFlightStore,
    store: &EntityStore,
    in_flight_per_key: u32,
    retry_blacklist: &[KeyId],
    required_caps: Option<&RequiredCapabilities>,
) -> Vec<Arc<VendorKey>> {
    let mut out = Vec::with_capacity(group.vendor_key_ids.len());
    let now_unix_s = now_unix_s();
    for kid in &group.vendor_key_ids {
        let Some(v) = store.vendor_key_or_load(kid).await else {
            continue;
        };
        let key = &*v.value;
        if !matches!(key.status, KeyStatus::Active) {
            continue;
        }
        if !key.quota_state.is_routable_at_unix_s(now_unix_s) {
            continue;
        }
        if !key.accessible_models.contains(model) {
            continue;
        }
        if !(key.accessible_endpoints.contains(&target_protocol) || key.protocol_conversion) {
            continue;
        }
        if !perm.allowed_channels.is_empty() && !perm.allowed_channels.contains(&key.key_id) {
            continue;
        }
        if in_flight.count(&key.key_id) >= in_flight_per_key {
            continue;
        }
        if retry_blacklist.iter().any(|k| k == &key.key_id) {
            continue;
        }
        // G13 — capability match. `None` disables the filter; a channel with no
        // advertised `forced_capabilities` is non-restrictive (see
        // `candidate_matches`).
        if let Some(req) = required_caps {
            let advertised = parse_capabilities(&key.forced_capabilities);
            if !candidate_matches(req, &advertised, key.exclusive_capability_match) {
                continue;
            }
        }
        out.push(Arc::new(VendorKey {
            key_id: key.key_id.clone(),
            vendor_id: key.vendor_id.clone(),
            api_key: SecretString::from(key.api_key.expose_secret().to_owned()),
            accessible_models: key.accessible_models.clone(),
            model_mapping: key.model_mapping.clone(),
            model_cost_weights: key.model_cost_weights.clone(),
            accessible_endpoints: key.accessible_endpoints.clone(),
            protocol_conversion: key.protocol_conversion,
            priority: key.priority,
            weight: key.weight,
            rpm_limit: key.rpm_limit,
            tpm_limit: key.tpm_limit,
            max_usage_count: key.max_usage_count,
            proxy: key.proxy.clone(),
            ip_allowlist: key.ip_allowlist.clone(),
            expires_at: key.expires_at,
            status: key.status,
            tags: key.tags.clone(),
            forced_capabilities: key.forced_capabilities.clone(),
            exclusive_capability_match: key.exclusive_capability_match,
            tunnel_preference: key.tunnel_preference.clone(),
            oauth: key.oauth.clone(),
            identity_snapshot: key.identity_snapshot.clone(),
            quota_state: key.quota_state.clone(),
        }));
    }
    out
}

fn now_unix_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}
