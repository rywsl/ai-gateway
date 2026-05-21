//! CapabilityFilter — single-shot per-(required, candidate-vendor-key) match
//! for the routing filter chain. Returns 4 outcomes: Match, Missing, ExtraCaps,
//! ApiKeyDisabled.
//!
//! `RedosTracker` integration: a candidate ApiKey that has tripped the ReDoS
//! budget enough times is reported as `ApiKeyDisabled`, short-circuiting all
//! downstream filters.

use crate::types::{Capability, MatchMode, RequiredCapabilities};
use gw_auth::model_permissions::RedosTracker;
use smallvec::SmallVec;
use std::sync::Arc;

pub struct CapabilityFilter {
    redos: Arc<RedosTracker>,
}

impl CapabilityFilter {
    pub fn new(redos: Arc<RedosTracker>) -> Self {
        Self { redos }
    }

    pub fn evaluate(
        &self,
        required: &RequiredCapabilities,
        candidate: &CandidateView<'_>,
    ) -> CapabilityFilterResult {
        if self.redos.is_disabled(candidate.api_key_id) {
            return CapabilityFilterResult::ApiKeyDisabled;
        }
        let mut missing: SmallVec<[Capability; 4]> = SmallVec::new();
        for c in &required.set {
            if !candidate.detected_capabilities.contains(c) {
                missing.push(*c);
            }
        }
        if !missing.is_empty() {
            return CapabilityFilterResult::Missing(missing);
        }
        if required.mode == MatchMode::Exclusive {
            for c in &candidate.detected_capabilities {
                if !required.set.contains(c) {
                    return CapabilityFilterResult::ExtraCaps;
                }
            }
        }
        metrics::counter!("gw_capability_resolve_total", "match" => "filter_hit").increment(1);
        CapabilityFilterResult::Match
    }
}

pub struct CandidateView<'a> {
    pub api_key_id: &'a str,
    pub vendor_key_id: &'a str,
    pub detected_capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityFilterResult {
    Match,
    Missing(SmallVec<[Capability; 4]>),
    /// Exclusive mode: candidate has caps NOT in required.
    ExtraCaps,
    /// ApiKey auto-disabled by ReDoS tracker.
    ApiKeyDisabled,
}
