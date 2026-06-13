//! CapabilityResolver — turns request hints (model, key forced caps, header,
//! retry upgrade) into a `RequiredCapabilities` set with origin provenance.
//!
//! Header strings parse via a fixed name → Capability table. Unknown header
//! strings collapse to `Capability::Tagged(Arc<str>)` via a bounded intern
//! table — distinct strings share a single `Arc`, and the table capacity
//! (10_000) bounds the worst-case memory for unknown capability strings.

use crate::types::{Capability, CapabilityOrigin, MatchMode, RequiredCapabilities};
use async_trait::async_trait;
use gw_core::error::ErrorClass;
use gw_core::protocol::{ApiShape, ProtocolKind};
use moka::sync::Cache;
use parking_lot::Mutex;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;

/// Bounded intern table for `Capability::Tagged` strings. Maps a tag
/// string to a shared `Arc<str>` so distinct requests with the same
/// tag share storage. Capacity-bounded (10_000); does NOT use TTI
/// because the resulting `Arc<str>` is embedded in `Capability::Tagged`
/// values held by callers — TTI eviction would not free those, only
/// orphan the intern slot. Max-capacity LRU eviction is acceptable:
/// in steady state an evicted tag is re-interned to a new (but
/// semantically equivalent) `Arc`. Equality / hash on `Capability`
/// uses `Arc<str>`'s `str` comparison, so re-interned Arcs match.
const CAPABILITY_INTERN_CAPACITY: u64 = 10_000;

static CAPABILITY_INTERN: LazyLock<Cache<Box<str>, Arc<str>>> = LazyLock::new(|| {
    Cache::builder()
        .max_capacity(CAPABILITY_INTERN_CAPACITY)
        .build()
});

/// Intern a tag string. Returns a shared `Arc<str>` if the cache had
/// it; otherwise creates a fresh `Arc<str>` and stores it. Even when
/// evicted (capacity overflow) and re-interned, two `Capability::Tagged`
/// values with the same str content compare equal via `Arc<str>` deref.
fn intern_tag(s: &str) -> Arc<str> {
    let key: Box<str> = s.into();
    CAPABILITY_INTERN.get_with(key, || Arc::<str>::from(s))
}

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
                r.add(c.clone(), CapabilityOrigin::RetryUpgrade);
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

pub(crate) fn parse_capability(s: &str) -> Capability {
    match s.to_ascii_lowercase().as_str() {
        "vision" => Capability::Vision,
        "audio" => Capability::Audio,
        "video" => Capability::Video,
        "thinking" => Capability::Thinking,
        "tool_use" => Capability::ToolUse,
        "streaming_only" => Capability::StreamingOnly,
        "openai_responses_shape" => Capability::OpenAiResponsesShape,
        "vertex_ai_service_account" => Capability::VertexAiServiceAccount,
        other => Capability::Tagged(intern_tag(other)),
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

#[cfg(test)]
mod intern_tests {
    use super::*;

    #[test]
    fn parse_capability_returns_known_variant_for_known_string() {
        assert_eq!(parse_capability("vision"), Capability::Vision);
        assert_eq!(parse_capability("thinking"), Capability::Thinking);
    }

    /// Contract lock (audit P0 #13 §4): every well-known capability name must
    /// resolve to its typed `Capability` variant, NOT `Capability::Tagged`.
    /// `parse_capability` is the single string→`Capability` table in the
    /// workspace (`backend.rs`, `filter.rs::parse_capabilities`, and the
    /// resolver header path all funnel through it), so this guards against a
    /// second, divergent mapping appearing and against a known name silently
    /// regressing to an interned tag (which would fail to match a typed,
    /// hard-constructed requirement such as `OpenAiResponsesShape`). Matching
    /// is case-insensitive.
    #[test]
    fn parse_capability_locks_all_known_names_to_typed_variants() {
        let known: &[(&str, Capability)] = &[
            ("vision", Capability::Vision),
            ("audio", Capability::Audio),
            ("video", Capability::Video),
            ("thinking", Capability::Thinking),
            ("tool_use", Capability::ToolUse),
            ("streaming_only", Capability::StreamingOnly),
            ("openai_responses_shape", Capability::OpenAiResponsesShape),
            (
                "vertex_ai_service_account",
                Capability::VertexAiServiceAccount,
            ),
        ];
        for (name, expected) in known {
            assert_eq!(
                &parse_capability(name),
                expected,
                "{name} must map to its typed variant"
            );
            // Case folding holds, and no known name leaks into Tagged.
            assert_eq!(&parse_capability(&name.to_ascii_uppercase()), expected);
            assert!(
                !matches!(parse_capability(name), Capability::Tagged(_)),
                "{name} must not collapse to Tagged"
            );
        }
    }

    #[test]
    fn parse_capability_interns_unknown_string() {
        let a = parse_capability("custom-foo");
        let b = parse_capability("custom-foo");
        // Both should equal each other and contain the same string.
        assert_eq!(a, b);
        match (&a, &b) {
            (Capability::Tagged(arc_a), Capability::Tagged(arc_b)) => {
                // Critical: the SAME Arc is returned (pointer equality on
                // the underlying str), proving the intern table is shared.
                assert!(
                    Arc::ptr_eq(arc_a, arc_b),
                    "Tagged Arcs for identical strings must be the same Arc"
                );
            }
            _ => panic!("expected Tagged variants"),
        }
    }

    #[test]
    fn intern_table_capacity_bounded_under_attack() {
        // Push past capacity. Equality still holds via Arc<str> content
        // comparison even after LRU eviction re-interns.
        for i in 0..(CAPABILITY_INTERN_CAPACITY as usize + 100) {
            let _ = parse_capability(&format!("attack-tag-{i}"));
        }
        CAPABILITY_INTERN.run_pending_tasks();
        assert!(
            CAPABILITY_INTERN.entry_count() <= CAPABILITY_INTERN_CAPACITY,
            "intern table must cap at CAPABILITY_INTERN_CAPACITY, got {}",
            CAPABILITY_INTERN.entry_count()
        );
    }
}
