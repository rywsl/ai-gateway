use gw_capability::resolver::{CapabilityResolver, FakeResolveBackend};
use gw_capability::types::Capability;
use gw_core::protocol::{ApiShape, ProtocolKind};
use std::sync::Arc;

#[tokio::test]
async fn responses_shape_auto_adds_cap() {
    let be = FakeResolveBackend::default();
    let r = CapabilityResolver::new(Arc::new(be));
    let got = r
        .resolve_with_shape(
            "ak1",
            "gpt-5",
            None,
            ProtocolKind::OpenAi,
            ApiShape::OpenAiResponses,
            None,
        )
        .await
        .unwrap();
    assert!(got.set.contains(&Capability::OpenAiResponsesShape));
}

#[tokio::test]
async fn chat_shape_does_not_add_responses_cap() {
    let be = FakeResolveBackend::default();
    let r = CapabilityResolver::new(Arc::new(be));
    let got = r
        .resolve_with_shape(
            "ak1",
            "gpt-5",
            None,
            ProtocolKind::OpenAi,
            ApiShape::OpenAiChat,
            None,
        )
        .await
        .unwrap();
    assert!(!got.set.contains(&Capability::OpenAiResponsesShape));
}

#[tokio::test]
async fn anthropic_shape_does_not_add_responses_cap() {
    let be = FakeResolveBackend::default();
    let r = CapabilityResolver::new(Arc::new(be));
    let got = r
        .resolve_with_shape(
            "ak1",
            "claude",
            None,
            ProtocolKind::Anthropic,
            ApiShape::AnthropicMessages,
            None,
        )
        .await
        .unwrap();
    assert!(!got.set.contains(&Capability::OpenAiResponsesShape));
}

#[tokio::test]
async fn responses_shape_with_existing_caps_preserves_them() {
    let be = FakeResolveBackend::default();
    be.set_model_caps("gpt-5", vec![Capability::Vision]);
    let r = CapabilityResolver::new(Arc::new(be));
    let got = r
        .resolve_with_shape(
            "ak1",
            "gpt-5",
            None,
            ProtocolKind::OpenAi,
            ApiShape::OpenAiResponses,
            None,
        )
        .await
        .unwrap();
    assert!(got.set.contains(&Capability::Vision));
    assert!(got.set.contains(&Capability::OpenAiResponsesShape));
}
