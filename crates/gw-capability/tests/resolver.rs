use gw_capability::resolver::{
    CapabilityResolver, FakeResolveBackend, ResolveBackend, ResolveRequest, RetryUpgradeHint,
};
use gw_capability::types::{Capability, CapabilityOrigin, MatchMode};
use gw_core::error::ErrorClass;
use gw_core::protocol::ProtocolKind;
use smallvec::smallvec;
use std::sync::Arc;

// Reference `ResolveBackend` so the import — exercised via dyn-trait usage in
// production callers — stays in the test module without tripping unused-import.
const _: fn(Arc<dyn ResolveBackend>) = |_| {};

#[tokio::test]
async fn model_caps_merge_with_header_require() {
    let be = FakeResolveBackend::default();
    be.set_model_caps("claude-3-opus", vec![Capability::Vision]);
    let r = CapabilityResolver::new(Arc::new(be));
    let header = ["thinking"];
    let req = ResolveRequest {
        api_key_id: "ak1",
        model_alias: "claude-3-opus",
        header_require: Some(&header),
        protocol: ProtocolKind::Anthropic,
        retry_upgrade: None,
    };
    let got = r.resolve(&req).await.unwrap();
    assert!(got.set.contains(&Capability::Vision));
    assert!(got.set.contains(&Capability::Thinking));
    assert_eq!(got.mode, MatchMode::Compatible);
}

#[tokio::test]
async fn key_forced_caps_take_origin() {
    let be = FakeResolveBackend::default();
    be.set_key_forced("ak1", vec![Capability::Audio]);
    let r = CapabilityResolver::new(Arc::new(be));
    let req = ResolveRequest {
        api_key_id: "ak1",
        model_alias: "any",
        header_require: None,
        protocol: ProtocolKind::OpenAi,
        retry_upgrade: None,
    };
    let got = r.resolve(&req).await.unwrap();
    assert!(got.set.contains(&Capability::Audio));
    assert!(got
        .provenance
        .iter()
        .any(|(_, o)| *o == CapabilityOrigin::KeyForced));
}

#[tokio::test]
async fn retry_upgrade_adds_thinking() {
    let be = FakeResolveBackend::default();
    let r = CapabilityResolver::new(Arc::new(be));
    let hint = RetryUpgradeHint {
        last_failure_class: ErrorClass::ProtocolError,
        upgrade_path: smallvec![Capability::Thinking],
    };
    let req = ResolveRequest {
        api_key_id: "ak1",
        model_alias: "any",
        header_require: None,
        protocol: ProtocolKind::Anthropic,
        retry_upgrade: Some(&hint),
    };
    let got = r.resolve(&req).await.unwrap();
    assert!(got.set.contains(&Capability::Thinking));
    assert!(got
        .provenance
        .iter()
        .any(|(_, o)| *o == CapabilityOrigin::RetryUpgrade));
}

#[tokio::test]
async fn exclusive_mode_inherits_from_key() {
    let be = FakeResolveBackend::default();
    be.set_match_mode_exclusive("ak1", true);
    let r = CapabilityResolver::new(Arc::new(be));
    let header = ["thinking"];
    let req = ResolveRequest {
        api_key_id: "ak1",
        model_alias: "x",
        header_require: Some(&header),
        protocol: ProtocolKind::OpenAi,
        retry_upgrade: None,
    };
    let got = r.resolve(&req).await.unwrap();
    assert_eq!(got.mode, MatchMode::Exclusive);
}

#[tokio::test]
async fn empty_request_returns_empty_set() {
    let be = FakeResolveBackend::default();
    let r = CapabilityResolver::new(Arc::new(be));
    let req = ResolveRequest {
        api_key_id: "ak1",
        model_alias: "any",
        header_require: None,
        protocol: ProtocolKind::OpenAi,
        retry_upgrade: None,
    };
    let got = r.resolve(&req).await.unwrap();
    assert!(got.set.is_empty());
}
