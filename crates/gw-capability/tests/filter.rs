#![allow(clippy::field_reassign_with_default)]

use gw_auth::model_permissions::RedosTracker;
use gw_capability::filter::{CandidateView, CapabilityFilter, CapabilityFilterResult};
use gw_capability::types::{Capability, CapabilityOrigin, MatchMode, RequiredCapabilities};
use std::sync::Arc;

#[test]
fn key_disabled_by_redos_returns_unavailable() {
    let tracker = Arc::new(RedosTracker::new());
    for _ in 0..10 {
        tracker.record_timeout("ak-bad");
    }
    let f = CapabilityFilter::new(tracker);
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Vision, CapabilityOrigin::Model);
    let candidate = CandidateView {
        api_key_id: "ak-bad",
        vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision],
    };
    assert_eq!(
        f.evaluate(&req, &candidate),
        CapabilityFilterResult::ApiKeyDisabled
    );
}

#[test]
fn compatible_mode_accepts_superset() {
    let f = CapabilityFilter::new(Arc::new(RedosTracker::new()));
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Vision, CapabilityOrigin::Model);
    req.mode = MatchMode::Compatible;
    let candidate = CandidateView {
        api_key_id: "ak1",
        vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision, Capability::Thinking],
    };
    assert_eq!(f.evaluate(&req, &candidate), CapabilityFilterResult::Match);
}

#[test]
fn exclusive_mode_rejects_superset() {
    let f = CapabilityFilter::new(Arc::new(RedosTracker::new()));
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Vision, CapabilityOrigin::Model);
    req.mode = MatchMode::Exclusive;
    let candidate = CandidateView {
        api_key_id: "ak1",
        vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision, Capability::Thinking],
    };
    assert_eq!(
        f.evaluate(&req, &candidate),
        CapabilityFilterResult::ExtraCaps
    );
}

#[test]
fn missing_required_cap_returns_missing() {
    let f = CapabilityFilter::new(Arc::new(RedosTracker::new()));
    let mut req = RequiredCapabilities::default();
    req.add(Capability::Audio, CapabilityOrigin::HeaderRequire);
    let candidate = CandidateView {
        api_key_id: "ak1",
        vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision],
    };
    match f.evaluate(&req, &candidate) {
        CapabilityFilterResult::Missing(v) => assert_eq!(v[..], [Capability::Audio]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn empty_required_matches_anything() {
    let f = CapabilityFilter::new(Arc::new(RedosTracker::new()));
    let req = RequiredCapabilities::default();
    let candidate = CandidateView {
        api_key_id: "ak1",
        vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision],
    };
    assert_eq!(f.evaluate(&req, &candidate), CapabilityFilterResult::Match);
}

#[test]
fn exclusive_mode_with_empty_required_rejects_anything_present() {
    let f = CapabilityFilter::new(Arc::new(RedosTracker::new()));
    let mut req = RequiredCapabilities::default();
    req.mode = MatchMode::Exclusive;
    let candidate = CandidateView {
        api_key_id: "ak1",
        vendor_key_id: "vk1",
        detected_capabilities: vec![Capability::Vision],
    };
    assert_eq!(
        f.evaluate(&req, &candidate),
        CapabilityFilterResult::ExtraCaps
    );
}
