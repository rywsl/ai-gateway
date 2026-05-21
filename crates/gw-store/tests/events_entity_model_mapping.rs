//! T4-D: events:entity invalidate extension for `ns=model_mapping`.
//!
//! These tests drive the `apply_entity_event` path inline via the
//! `apply_inline_for_test_helper` test-only entry point, so they need
//! neither Redis nor a live `StreamConsumer`.

use gw_core::entity::{ModelAvailability, ModelMapping};
use gw_core::protocol::ProtocolKind;
use gw_core::versioned::Versioned;
use gw_store::stream_consumer::apply_inline_for_test_helper;
use gw_store::EntityStore;
use std::sync::Arc;

fn seed_mapping(alias: &str, proto: ProtocolKind) -> ModelMapping {
    ModelMapping {
        user_alias: alias.to_string(),
        protocol: proto,
        default_upstream_model: format!("{alias}-upstream"),
        vendor_key_overrides: Default::default(),
        availability: ModelAvailability::Enabled,
        pricing_strategy_ref: None,
    }
}

#[tokio::test]
async fn invalidates_model_mapping_on_event() {
    let store = Arc::new(EntityStore::new());
    let key = ("claude-3-opus".to_string(), ProtocolKind::Anthropic);
    store
        .model_mapping
        .insert(
            key.clone(),
            Arc::new(Versioned::new(
                1,
                seed_mapping("claude-3-opus", ProtocolKind::Anthropic),
            )),
        )
        .await;
    assert!(store.model_mapping.get(&key).await.is_some(), "seed");

    apply_inline_for_test_helper(&store, "model_mapping", "claude-3-opus|anthropic").await;

    assert!(
        store.model_mapping.get(&key).await.is_none(),
        "model_mapping cache should be invalidated"
    );
}

#[tokio::test]
async fn invalidates_each_supported_protocol() {
    let store = Arc::new(EntityStore::new());
    for (alias, proto, tag) in [
        ("gpt-4o", ProtocolKind::OpenAi, "openai"),
        ("claude-3-opus", ProtocolKind::Anthropic, "anthropic"),
        ("gemini-1.5-pro", ProtocolKind::Gemini, "gemini"),
    ] {
        let key = (alias.to_string(), proto);
        store
            .model_mapping
            .insert(
                key.clone(),
                Arc::new(Versioned::new(1, seed_mapping(alias, proto))),
            )
            .await;
        assert!(store.model_mapping.get(&key).await.is_some());

        apply_inline_for_test_helper(&store, "model_mapping", &format!("{alias}|{tag}")).await;

        assert!(
            store.model_mapping.get(&key).await.is_none(),
            "expected invalidate for {alias}|{tag}"
        );
    }
}

#[tokio::test]
async fn unknown_protocol_is_no_op() {
    let store = Arc::new(EntityStore::new());
    let key = ("claude-3-opus".to_string(), ProtocolKind::Anthropic);
    store
        .model_mapping
        .insert(
            key.clone(),
            Arc::new(Versioned::new(
                1,
                seed_mapping("claude-3-opus", ProtocolKind::Anthropic),
            )),
        )
        .await;

    apply_inline_for_test_helper(&store, "model_mapping", "claude-3-opus|cohere").await;

    assert!(
        store.model_mapping.get(&key).await.is_some(),
        "unknown protocol must not touch the cache"
    );
}

#[tokio::test]
async fn malformed_id_without_separator_does_not_panic() {
    let store = Arc::new(EntityStore::new());
    apply_inline_for_test_helper(&store, "model_mapping", "no-protocol-separator").await;
}

#[tokio::test]
async fn unknown_ns_is_no_op() {
    let store = Arc::new(EntityStore::new());
    apply_inline_for_test_helper(&store, "totally_unknown_ns", "x").await;
}
