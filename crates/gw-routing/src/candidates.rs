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
//! 5. the key's in-flight count is under the per-key cap.

use crate::breaker::BreakerStore;
use gw_core::entity::{KeyStatus, ModelGroup, VendorKey};
use gw_core::ids::ModelName;
use gw_core::permissions::MergedPermissions;
use gw_core::protocol::ProtocolKind;
use gw_store::entity_store::EntityStore;
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;

pub async fn get_candidate_keys(
    group: &ModelGroup,
    model: &ModelName,
    target_protocol: ProtocolKind,
    perm: &MergedPermissions,
    breakers: &BreakerStore,
    store: &EntityStore,
    breaker_in_flight_per_key: u32,
) -> Vec<Arc<VendorKey>> {
    let mut out = Vec::with_capacity(group.vendor_key_ids.len());
    for kid in &group.vendor_key_ids {
        let Some(v) = store.vendor_key.get(kid).await else {
            continue;
        };
        let key = &*v.value;
        if !matches!(key.status, KeyStatus::Active) {
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
        if breakers.in_flight(&key.key_id) >= breaker_in_flight_per_key {
            continue;
        }
        // Rebuild as Arc<VendorKey>: the store yields Arc<Versioned<VendorKey>>,
        // and VendorKey is not Clone (holds SecretString). We clone the secret
        // string contents explicitly via ExposeSecret.
        out.push(Arc::new(VendorKey {
            key_id: key.key_id.clone(),
            vendor_id: key.vendor_id.clone(),
            api_key: SecretString::from(key.api_key.expose_secret().to_owned()),
            accessible_models: key.accessible_models.clone(),
            model_mapping: key.model_mapping.clone(),
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
        }));
    }
    out
}
