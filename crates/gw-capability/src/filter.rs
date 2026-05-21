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

/// Parse a slice of capability name strings (as stored on `VendorKey` /
/// `ApiKeyEntry`) into the typed [`Capability`] set. Unknown names collapse to
/// `Capability::Tagged` via the shared resolver parser.
pub fn parse_capabilities<S: AsRef<str>>(names: &[S]) -> Vec<Capability> {
    names
        .iter()
        .map(|s| crate::resolver::parse_capability(s.as_ref()))
        .collect()
}

/// Pure capability match for a routing candidate (G13, ADR-G13-2). No ReDoS /
/// ApiKey-disable check (that lives on the ApiKey-keyed [`CapabilityFilter`]);
/// this operates purely on the candidate channel's advertised capability set.
///
/// Semantics:
/// - A candidate with an **empty** advertised set is non-restrictive: it
///   matches any requirement set.
/// - Otherwise every required cap must be present in `candidate_caps`
///   (Compatible mode).
/// - In `Exclusive` mode the candidate must additionally advertise **no** cap
///   outside the required set (exact-match channels).
pub fn candidate_matches(
    required: &RequiredCapabilities,
    candidate_caps: &[Capability],
    candidate_exclusive: bool,
) -> bool {
    if candidate_caps.is_empty() {
        return true;
    }
    for c in &required.set {
        if !candidate_caps.contains(c) {
            return false;
        }
    }
    let exclusive = candidate_exclusive || required.mode == MatchMode::Exclusive;
    if exclusive {
        for c in candidate_caps {
            if !required.set.contains(c) {
                return false;
            }
        }
    }
    true
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
