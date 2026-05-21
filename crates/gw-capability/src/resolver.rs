//! CapabilityResolver — turns request hints (model, key forced caps, header,
//! retry upgrade) into a `RequiredCapabilities` set with origin provenance.
//!
//! Header strings parse via a fixed name → Capability table. Unknown header
//! strings collapse to `Capability::Tagged(&'static str)` via a leak — this is
//! deliberately bounded (callers must validate header allowlist before reaching
//! us in production paths). For unit-test purposes this is fine.

use crate::types::{Capability, CapabilityOrigin, MatchMode, RequiredCapabilities};
use async_trait::async_trait;
use gw_core::error::ErrorClass;
use gw_core::protocol::{ApiShape, ProtocolKind};
use parking_lot::Mutex;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::Arc;

pub struct CapabilityResolver {
    backend: Arc<dyn ResolveBackend>,
}

pub struct ResolveRequest<'a> {
    pub api_key_id: &'a str,
    pub model_alias: &'a str,
    pub header_require: Option<&'a [&'a str]>,
    pub protocol: ProtocolKind,
    pub retry_upgrade: Option<&'a RetryUpgradeHint>,
}

#[derive(Debug, Clone)]
pub struct RetryUpgradeHint {
    pub last_failure_class: ErrorClass,
    pub upgrade_path: SmallVec<[Capability; 2]>,
}

#[async_trait]
pub trait ResolveBackend: Send + Sync {
    async fn model_caps(&self, model_alias: &str, protocol: ProtocolKind) -> Vec<Capability>;
    async fn key_forced_caps(&self, api_key_id: &str) -> Vec<Capability>;
    async fn match_mode_exclusive(&self, api_key_id: &str) -> bool;
}

impl CapabilityResolver {
    pub fn new(backend: Arc<dyn ResolveBackend>) -> Self {
        Self { backend }
    }

    pub async fn resolve(&self, req: &ResolveRequest<'_>) -> anyhow::Result<RequiredCapabilities> {
        let mut r = RequiredCapabilities::default();
        for c in self.backend.key_forced_caps(req.api_key_id).await {
            r.add(c, CapabilityOrigin::KeyForced);
        }
        for c in self.backend.model_caps(req.model_alias, req.protocol).await {
            r.add(c, CapabilityOrigin::Model);
        }
        if let Some(hs) = req.header_require {
            for s in hs {
                r.add(parse_capability(s), CapabilityOrigin::HeaderRequire);
            }
        }
        if let Some(hint) = req.retry_upgrade {
            for c in &hint.upgrade_path {
                r.add(*c, CapabilityOrigin::RetryUpgrade);
            }
        }
        r.mode = if self.backend.match_mode_exclusive(req.api_key_id).await {
            MatchMode::Exclusive
        } else {
            MatchMode::Compatible
        };
        let label = if r.set.is_empty() { "miss" } else { "hit" };
        metrics::counter!("gw_capability_resolve_total", "match" => label).increment(1);
        Ok(r)
    }

    /// Resolve capabilities and auto-attach shape-derived caps.
    ///
    /// `shape == ApiShape::OpenAiResponses` adds `Capability::OpenAiResponsesShape`
    /// with `CapabilityOrigin::HeaderRequire` (treating the shape detection as
    /// equivalent to a client-supplied requirement, so origin tracking shows
    /// where the cap came from).
    pub async fn resolve_with_shape(
        &self,
        api_key_id: &str,
        model_alias: &str,
        header_require: Option<&[&str]>,
        protocol: ProtocolKind,
        shape: ApiShape,
        retry_upgrade: Option<&RetryUpgradeHint>,
    ) -> anyhow::Result<RequiredCapabilities> {
        let req = ResolveRequest {
            api_key_id,
            model_alias,
            header_require,
            protocol,
            retry_upgrade,
        };
        let mut r = self.resolve(&req).await?;
        if shape == ApiShape::OpenAiResponses {
            r.add(
                Capability::OpenAiResponsesShape,
                CapabilityOrigin::HeaderRequire,
            );
        }
        Ok(r)
    }
}

fn parse_capability(s: &str) -> Capability {
    match s.to_ascii_lowercase().as_str() {
        "vision" => Capability::Vision,
        "audio" => Capability::Audio,
        "video" => Capability::Video,
        "thinking" => Capability::Thinking,
        "tool_use" => Capability::ToolUse,
        "streaming_only" => Capability::StreamingOnly,
        "openai_responses_shape" => Capability::OpenAiResponsesShape,
        "vertex_ai_service_account" => Capability::VertexAiServiceAccount,
        other => Capability::Tagged(Box::leak(other.to_string().into_boxed_str())),
    }
}

/// In-memory test backend.
#[derive(Default)]
pub struct FakeResolveBackend {
    model_caps: Mutex<HashMap<(String, ProtocolKind), Vec<Capability>>>,
    key_forced: Mutex<HashMap<String, Vec<Capability>>>,
    exclusive_keys: Mutex<HashMap<String, bool>>,
}

impl FakeResolveBackend {
    pub fn set_model_caps(&self, model: &str, caps: Vec<Capability>) {
        let mut g = self.model_caps.lock();
        g.insert((model.into(), ProtocolKind::Anthropic), caps.clone());
        g.insert((model.into(), ProtocolKind::OpenAi), caps.clone());
        g.insert((model.into(), ProtocolKind::Gemini), caps);
    }

    pub fn set_key_forced(&self, ak: &str, caps: Vec<Capability>) {
        self.key_forced.lock().insert(ak.into(), caps);
    }

    pub fn set_match_mode_exclusive(&self, ak: &str, v: bool) {
        self.exclusive_keys.lock().insert(ak.into(), v);
    }
}

#[async_trait]
impl ResolveBackend for FakeResolveBackend {
    async fn model_caps(&self, m: &str, p: ProtocolKind) -> Vec<Capability> {
        self.model_caps
            .lock()
            .get(&(m.to_string(), p))
            .cloned()
            .unwrap_or_default()
    }
    async fn key_forced_caps(&self, ak: &str) -> Vec<Capability> {
        self.key_forced.lock().get(ak).cloned().unwrap_or_default()
    }
    async fn match_mode_exclusive(&self, ak: &str) -> bool {
        *self.exclusive_keys.lock().get(ak).unwrap_or(&false)
    }
}
