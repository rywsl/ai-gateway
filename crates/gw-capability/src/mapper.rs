//! Model alias → upstream model mapper (T4-A).
//!
//! Drives both routing-time selection (where availability gates apply) and
//! dispatch-time picking (where vendor-key overrides are consulted).
//! Production wiring against `EntityStore` lands in Wave 6; this module
//! ships the trait + an in-memory Fake for unit tests.

use async_trait::async_trait;
use gw_core::entity::{ModelAvailability, ModelMapping};
use gw_core::protocol::ProtocolKind;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingSource {
    EntityRow,
    Identity,
    PerKey,
}

#[derive(Debug, Clone)]
pub struct MappedModel {
    pub user_alias: String,
    pub upstream_model: String,
    pub source: MappingSource,
}

#[derive(Debug, thiserror::Error)]
pub enum MapperError {
    #[error("model disabled: {0}")]
    Disabled(String),
    #[error("backend: {0}")]
    Backend(String),
}

#[async_trait]
pub trait MapperBackend: Send + Sync {
    async fn get(&self, user_alias: &str, protocol: ProtocolKind) -> Option<ModelMapping>;
}

pub struct ModelMapper {
    backend: Arc<dyn MapperBackend>,
}

impl ModelMapper {
    pub fn new(backend: Arc<dyn MapperBackend>) -> Self {
        Self { backend }
    }

    pub async fn map_for_routing(
        &self,
        user_alias: &str,
        protocol: ProtocolKind,
    ) -> Result<MappedModel, MapperError> {
        let mm = self.backend.get(user_alias, protocol).await;
        let Some(mm) = mm else {
            return Ok(MappedModel {
                user_alias: user_alias.to_string(),
                upstream_model: user_alias.to_string(),
                source: MappingSource::Identity,
            });
        };
        match mm.availability {
            ModelAvailability::Disabled => return Err(MapperError::Disabled(user_alias.into())),
            // ShadowOnly is transparent here; routing layer handles internal-caller checks.
            ModelAvailability::ShadowOnly | ModelAvailability::Enabled => {}
        }
        let source = if mm.vendor_key_overrides.is_empty() {
            MappingSource::EntityRow
        } else {
            MappingSource::PerKey
        };
        Ok(MappedModel {
            user_alias: mm.user_alias,
            upstream_model: mm.default_upstream_model,
            source,
        })
    }

    pub async fn map_for_dispatch(
        &self,
        user_alias: &str,
        protocol: ProtocolKind,
        vendor_key_id: &str,
    ) -> String {
        if let Some(mm) = self.backend.get(user_alias, protocol).await {
            if let Some(o) = mm.vendor_key_overrides.get(vendor_key_id) {
                return o.clone();
            }
            return mm.default_upstream_model;
        }
        user_alias.to_string()
    }
}

/// In-memory FakeMapperBackend for unit tests.
#[derive(Default)]
pub struct FakeMapperBackend {
    map: Mutex<HashMap<(String, ProtocolKind), ModelMapping>>,
}

impl FakeMapperBackend {
    pub fn insert(&self, m: ModelMapping) {
        self.map.lock().insert((m.user_alias.clone(), m.protocol), m);
    }
}

#[async_trait]
impl MapperBackend for FakeMapperBackend {
    async fn get(&self, user_alias: &str, protocol: ProtocolKind) -> Option<ModelMapping> {
        self.map.lock().get(&(user_alias.to_string(), protocol)).cloned()
    }
}
