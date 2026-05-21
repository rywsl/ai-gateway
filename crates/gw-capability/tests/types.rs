use gw_capability::types::{Capability, CapabilityOrigin, MatchMode, RequiredCapabilities};

#[test]
fn required_capabilities_default_compatible() {
    let r = RequiredCapabilities::default();
    assert!(r.set.is_empty());
    assert_eq!(r.mode, MatchMode::Compatible);
}

#[test]
fn required_capabilities_provenance_logs_origin() {
    let mut r = RequiredCapabilities::default();
    r.add(Capability::Vision, CapabilityOrigin::Model);
    r.add(Capability::Thinking, CapabilityOrigin::HeaderRequire);
    assert_eq!(r.set.len(), 2);
    assert_eq!(r.provenance.len(), 2);
}

#[test]
fn add_dedups_capability_but_appends_origin() {
    let mut r = RequiredCapabilities::default();
    r.add(Capability::Vision, CapabilityOrigin::Model);
    r.add(Capability::Vision, CapabilityOrigin::HeaderRequire);
    assert_eq!(r.set.len(), 1);
    assert_eq!(r.provenance.len(), 2);
}

#[test]
fn match_mode_default_is_compatible() {
    assert_eq!(MatchMode::default(), MatchMode::Compatible);
}
