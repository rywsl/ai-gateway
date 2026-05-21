//! Production [`ResolveBackend`] (G13, ADR-G13-1) wiring the resolver to the
//! shared [`EntityStore`].
//!
//! Three lookups, all soft-fail to the non-restrictive default on a store
//! miss/timeout (ADR-G13-1: "treat as no specific requirements"):
//!
//! - `model_caps(model_alias, _)` — reads `Model.capabilities: Vec<String>`
//!   via [`EntityStore::model_or_load`] and parses each string into a
//!   [`Capability`].
//! - `key_forced_caps(api_key_id)` — reads `ApiKeyEntry.force_capabilities`
//!   via [`EntityStore::apikey_or_load`]. Note the parameter is an **ApiKey**
//!   id, not a vendor-key id: forced caps live on the caller's ApiKey
//!   (T4-B), not on the upstream channel. The per-channel
//!   `VendorKey.forced_capabilities` field feeds the candidate filter
//!   (`CapabilityFilter`), not the resolver.
//! - `match_mode_exclusive(api_key_id)` — reads
//!   `ApiKeyEntry.match_mode_exclusive`.
//!
//! Naming asymmetry (`force_capabilities`/`SmallVec` on `ApiKeyEntry` vs
//! `forced_capabilities`/`Vec` on `VendorKey`) is deliberate for now; see
//! `docs/architecture.md` §Deliberate behaviours.

use crate::resolver::{parse_capability, ResolveBackend};
use crate::types::Capability;
use async_trait::async_trait;
use gw_core::ids::{ApiKeyId, ModelName};
use gw_core::protocol::ProtocolKind;
use gw_store::entity_store::EntityStore;
use std::sync::Arc;

/// `ResolveBackend` backed by the gateway's shared [`EntityStore`].
pub struct EntityStoreResolveBackend {
    store: Arc<EntityStore>,
}

impl EntityStoreResolveBackend {
    pub fn new(store: Arc<EntityStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ResolveBackend for EntityStoreResolveBackend {
    async fn model_caps(&self, model_alias: &str, _protocol: ProtocolKind) -> Vec<Capability> {
        self.store
            .model_or_load(&ModelName::from(model_alias))
            .await
            .map(|m| {
                m.value
                    .capabilities
                    .iter()
                    .map(|s| parse_capability(s))
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn key_forced_caps(&self, api_key_id: &str) -> Vec<Capability> {
        self.store
            .apikey_or_load(&ApiKeyId::from(api_key_id))
            .await
            .map(|k| {
                k.value
                    .force_capabilities
                    .iter()
                    .map(|s| parse_capability(s))
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn match_mode_exclusive(&self, api_key_id: &str) -> bool {
        self.store
            .apikey_or_load(&ApiKeyId::from(api_key_id))
            .await
            .map(|k| k.value.match_mode_exclusive)
            .unwrap_or(false)
    }
}
