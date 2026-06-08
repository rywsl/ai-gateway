use gw_core::entity::EstimatePricing;
use gw_core::ids::ModelName;
use gw_core::versioned::Versioned;
use gw_store::EntityStore;
use std::sync::Arc;

#[tokio::test]
async fn estimate_pricing_cache_returns_inserted_snapshot() {
    let store = EntityStore::new();
    let model = ModelName::from("gpt-4o");
    store
        .estimate_pricing
        .insert(
            model.clone(),
            Arc::new(Versioned {
                version: 42,
                value: Arc::new(EstimatePricing {
                    model: model.clone(),
                    conservative_per_token_micro: 60,
                    safety_factor: 1.25,
                    default_max_tokens: 4096,
                    hard_cap_tokens: 200_000,
                }),
            }),
        )
        .await;

    let loaded = store.estimate_pricing_or_load(&model).await.unwrap();
    assert_eq!(loaded.version, 42);
    assert_eq!(loaded.value.conservative_per_token_micro, 60);
}

#[test]
fn estimate_pricing_deserializes_go_contract_payload() {
    let pricing: EstimatePricing = serde_json::from_value(serde_json::json!({
        "model": "gpt-4o",
        "conservative_per_token_micro": 60,
        "safety_factor": 1.1,
        "default_max_tokens": 4096,
        "hard_cap_tokens": 200000
    }))
    .unwrap();

    assert_eq!(pricing.model, ModelName::from("gpt-4o"));
    assert_eq!(pricing.hard_cap_tokens, 200000);
}
