//! AdapterCapabilityHook — adapter-injected per-vendor-key capability test.
//!
//! Fast path: hook reads `entity:vendor_key.detected_capabilities`.
//! Slow path (rare): hook may probe upstream (codex realtime quota, etc.) —
//! that's the adapter's choice; the trait is sync to keep the contract simple
//! at the routing filter chain. Adapters that need async probe should pre-fetch
//! out of band and stash results in EntityVendorKeyView.

use crate::types::{Capability, RequiredCapabilities};
use smallvec::SmallVec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityDetectResult {
    /// All required caps present on this vendor key.
    Match,
    /// Some required caps are missing on this vendor key.
    Missing(SmallVec<[Capability; 2]>),
    /// Adapter could not determine; routing should rank this key as `unknown`.
    Unknown,
}

pub struct DetectCtx<'a> {
    pub required: &'a RequiredCapabilities,
    pub vendor_key: &'a EntityVendorKeyView,
}

/// Read-only projection of `entity:vendor_key` for capability checks.
///
/// Production wiring populates this from `EntityStore`; tests use a literal.
pub struct EntityVendorKeyView {
    pub detected_capabilities: Vec<Capability>,
}

pub trait AdapterCapabilityHook: Send + Sync {
    fn detect(&self, ctx: &DetectCtx<'_>) -> CapabilityDetectResult;
}
